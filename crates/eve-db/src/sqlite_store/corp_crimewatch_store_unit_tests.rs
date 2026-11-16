//! Unit and mutation-killing tests for `sqlite_store::corp_crimewatch_store`.

use super::*;
use crate::sqlite_store::fingerprints::{canonical_fp, FnvHasher, FnvSerError, FnvSerializer};
use crate::sqlite_store::schema::apply_migrations;
use rusqlite::params;
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

fn temp_corp_crimewatch_db_path(tag: &str) -> std::path::PathBuf {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "evomu_corp_cw_{tag}_{}_{seq}.sqlite",
        std::process::id()
    ))
}

fn populate_all_family4_fields(snap: &mut WorldStateSnapshot) {
    // CorpGovernanceDomain (17 maps)
    snap.corp_governance.insert(
        98_000_001,
        CorpGovState {
            member_notes: HashMap::from([(90_000_001, "Director".into())]),
            ..Default::default()
        },
    );
    snap.employment_history
        .insert(90_000_001, vec![(98_000_001, 1_700_000_000, None)]);
    snap.corp_hangar_divisions.insert(
        98_000_001,
        HashMap::from([(1u8, "Main Hangar".into()), (2u8, "Ships".into())]),
    );
    snap.corp_alliance_history.insert(
        98_000_001,
        vec![CorpAllianceHistoryEntry {
            record_id: 1,
            alliance_id: Some(99_000_001),
            start_date_unix: 1_700_000_010,
            is_deleted: Some(false),
        }],
    );
    snap.corp_container_logs.insert(
        98_000_001,
        vec![CorpContainerLogEntry {
            logged_at_unix: 1_700_000_020,
            container_id: 700_001,
            action: "unlock".into(),
            ..Default::default()
        }],
    );
    snap.corp_medals
        .insert(98_000_001, vec![CorpMedalEntry::default()]);
    snap.corp_issued_medals
        .insert(98_000_001, vec![CorpIssuedMedalEntry::default()]);
    snap.corp_member_limits.insert(98_000_001, 50);
    snap.corp_titles
        .insert(98_000_001, vec![CorpTitleEntry::default()]);
    snap.corp_member_titles
        .insert((98_000_001, 90_000_001), vec![1, 2]);
    snap.corp_role_assignments
        .insert((98_000_001, 90_000_001), CorpMemberRoleAssignment::default());
    snap.corp_role_history.insert(
        98_000_001,
        vec![CorpRoleHistoryEntry {
            changed_at_unix: 1_700_000_030,
            issuer_id: 90_000_001,
            role_type: "roles".into(),
            ..Default::default()
        }],
    );
    snap.corp_shareholders.insert(
        98_000_001,
        vec![CorpShareholderEntry {
            shareholder_id: 90_000_001,
            shareholder_type: "character".into(),
            share_count: 1_000,
        }],
    );
    snap.char_medals.insert(
        90_000_001,
        vec![CharacterMedalRecord {
            medal_id: 1,
            title: "Hero".into(),
            ..Default::default()
        }],
    );
    snap.char_scoped_roles.insert(
        90_000_001,
        CharacterScopedRolesRecord {
            roles: vec!["Director".into()],
            ..Default::default()
        },
    );
    snap.corp_vote_cases.insert(
        501,
        CorpVoteCaseRecord {
            case_id: 501,
            corporation_id: 98_000_001,
            text: "Lease".into(),
            ..Default::default()
        },
    );
    snap.corp_vote_ballots.insert(
        (501, 90_000_001),
        CorpVoteBallotRecord {
            case_id: 501,
            voter_id: 90_000_001,
            option_id: 1,
            weight: 100,
            voted_at_unix: 1_700_000_040,
        },
    );

    // CrimewatchDomain (5 maps + combat_crimewatch_state)
    snap.crimewatch_states.insert(
        90_000_001,
        PersistedCrimeWatchState {
            character_id: 90_000_001,
            weapon_timer_until_unix: 12_345,
            ..Default::default()
        },
    );
    snap.crimewatch_aggressions.insert(
        (90_000_001, 90_000_002),
        PersistedAggressionRecord {
            aggressor_char_id: 90_000_001,
            victim_char_id: 90_000_002,
            system_id: 30_000_142,
            started_unix: 1_700_000_050,
            expires_unix: 1_700_000_950,
            is_legal: true,
            is_pod: false,
        },
    );
    snap.combat_crimewatch_state.safety_levels.insert(90_000_001, 1);
    snap.combat_crimewatch_state
        .limited_engagements
        .insert(90_000_001, HashMap::from([(90_000_002, 1_700_001_000)]));
    snap.combat_crimewatch_state
        .player_bounty_pool
        .insert(90_000_001, 150_000.5);
    snap.combat_crimewatch_state
        .duel_challenges
        .insert((90_000_001, 90_000_002), 1_700_000_060);
    snap.combat_crimewatch_state
        .corp_aggression_settings
        .insert(98_000_001, true);
    snap.wh_polarizations.insert((90_000_001, 31_000_001), 9999);
    snap.zarzakh_gate_locks
        .insert(90_000_001, (30_010_000, 12345));
    snap.structure_docking_acl_denied.insert(
        1_000_000_000_001,
        HashSet::from([90_000_005, 90_000_002, 90_000_009]),
    );

    // WarfareDomain (7 maps + fw_wars + 5 runtime substructs)
    snap.sov_prospecting_levels.insert(30_000_142, 3);
    snap.sov_campaign_scores.insert(
        1_000_000_000_020,
        SovCampaignScoreRecord {
            structure_id: 1_000_000_000_020,
            attackers_score: 0.25,
            defender_score: 0.75,
        },
    );
    snap.fw_character_stats.insert(
        90_000_001,
        FwCharacterStatsRecord {
            character_id: 90_000_001,
            current_rank: 4,
            ..Default::default()
        },
    );
    snap.fw_corp_enlisted_on.insert(98_000_001, 1_700_000_100);
    snap.fw_system_occupations.insert(
        30_000_142,
        FwSystemOccupationRecord {
            solar_system_id: 30_000_142,
            owner_faction_id: 500_001,
            occupier_faction_id: 500_001,
            victory_points: 100,
            victory_points_threshold: 3_000,
        },
    );
    snap.fw_wars.push((500_001, 500_002));
    snap.war_lifecycle_meta.insert(
        1,
        WarLifecycleMeta {
            open_for_allies: Some(true),
            ..Default::default()
        },
    );
    snap.alliance_lifecycle_meta.insert(
        99_000_001,
        AllianceLifecycleMeta {
            creator_id: 90_000_001,
            creator_corporation_id: 98_000_001,
            date_founded_unix: 1_700_000_000,
            faction_id: None,
        },
    );
    snap.alliance_war_state
        .project_meta
        .insert(801, ("Alpha".into(), true, false));
    snap.war_state.next_negotiation_id = 7;
    snap.war_state
        .mutual_invites
        .insert((98_000_001, 98_000_002), 1_700_000_070);
    snap.war_state.wars.insert(1, Default::default());
    snap.fw_runtime.system_victory_points.insert(30_000_142, 2_500);
    snap.sov_runtime
        .skyhook_active
        .insert(1_000_000_000_010, true);
    snap.sov_runtime.sov_upgrade_combat_npcs.insert(5_000_001);
    snap.insurgency_runtime
        .pirate_ambition_modifier
        .insert(4001, 3);
    snap.insurgency_runtime
        .active_state_notified
        .extend([90_000_001, 90_000_002]);
}

fn assert_family4_equal(lhs: &WorldStateSnapshot, rhs: &WorldStateSnapshot) {
    assert_eq!(lhs.corp_governance, rhs.corp_governance);
    assert_eq!(lhs.employment_history, rhs.employment_history);
    assert_eq!(lhs.corp_hangar_divisions, rhs.corp_hangar_divisions);
    assert_eq!(lhs.corp_alliance_history, rhs.corp_alliance_history);
    assert_eq!(lhs.corp_container_logs, rhs.corp_container_logs);
    assert_eq!(lhs.corp_medals, rhs.corp_medals);
    assert_eq!(lhs.corp_issued_medals, rhs.corp_issued_medals);
    assert_eq!(lhs.corp_member_limits, rhs.corp_member_limits);
    assert_eq!(lhs.corp_titles, rhs.corp_titles);
    assert_eq!(lhs.corp_member_titles, rhs.corp_member_titles);
    assert_eq!(lhs.corp_role_assignments, rhs.corp_role_assignments);
    assert_eq!(lhs.corp_role_history, rhs.corp_role_history);
    assert_eq!(lhs.corp_shareholders, rhs.corp_shareholders);
    assert_eq!(lhs.char_medals, rhs.char_medals);
    assert_eq!(lhs.char_scoped_roles, rhs.char_scoped_roles);
    assert_eq!(lhs.corp_vote_cases, rhs.corp_vote_cases);
    assert_eq!(lhs.corp_vote_ballots, rhs.corp_vote_ballots);
    assert_eq!(lhs.crimewatch_states, rhs.crimewatch_states);
    assert_eq!(lhs.crimewatch_aggressions, rhs.crimewatch_aggressions);
    assert_eq!(lhs.combat_crimewatch_state, rhs.combat_crimewatch_state);
    assert_eq!(lhs.wh_polarizations, rhs.wh_polarizations);
    assert_eq!(lhs.zarzakh_gate_locks, rhs.zarzakh_gate_locks);
    assert_eq!(lhs.structure_docking_acl_denied, rhs.structure_docking_acl_denied);
    assert_eq!(lhs.alliance_war_state, rhs.alliance_war_state);
    assert_eq!(lhs.war_state, rhs.war_state);
    assert_eq!(lhs.fw_runtime, rhs.fw_runtime);
    assert_eq!(lhs.sov_runtime, rhs.sov_runtime);
    assert_eq!(lhs.insurgency_runtime, rhs.insurgency_runtime);
    assert_eq!(lhs.sov_prospecting_levels, rhs.sov_prospecting_levels);
    assert_eq!(lhs.sov_campaign_scores, rhs.sov_campaign_scores);
    assert_eq!(lhs.fw_character_stats, rhs.fw_character_stats);
    assert_eq!(lhs.fw_corp_enlisted_on, rhs.fw_corp_enlisted_on);
    assert_eq!(lhs.fw_system_occupations, rhs.fw_system_occupations);
    assert_eq!(lhs.fw_wars, rhs.fw_wars);
    assert_eq!(lhs.war_lifecycle_meta, rhs.war_lifecycle_meta);
    assert_eq!(lhs.alliance_lifecycle_meta, rhs.alliance_lifecycle_meta);
}

#[test]
fn corp_crimewatch_domain_rows_full_roundtrip_sync_load_and_rollback() {
    use crate::sqlite_store::domain_rows::DomainRowChange;
    use crate::sqlite_store::quarantine::LoadContext;

    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    // Fresh DB uses `domain_rows` rather than legacy `d6_corp_crimewatch_kv`.
    let legacy_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'd6_corp_crimewatch_kv'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(legacy_count, 0);

    let mut snap = WorldStateSnapshot::default();
    populate_all_family4_fields(&mut snap);

    // Sync to SQLite (`domain_rows`), verify idempotence on immediate repeat.
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_corp_crimewatch_domain_rows(&tx, &snap).unwrap());
        assert!(!sync_corp_crimewatch_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }

    // Verify deterministic sorted ACL JSON inside `domain_rows` + decomposed scalar row.
    let acl_json: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'structure_docking_acl_denied/1000000000001'",
            params![DOMAIN_CRIMEWATCH],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(acl_json, "[90000002,90000005,90000009]");

    let neg_id_json: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'war_state.next_negotiation_id'",
            params![DOMAIN_WARFARE],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(neg_id_json, "7");

    // Load into a fresh snapshot with strict LoadContext and assert full 36-field parity.
    let mut loaded = WorldStateSnapshot::default();
    assert!(!load_corp_crimewatch_domain_rows(
        &conn,
        &mut loaded,
        &mut LoadContext::new(false),
    )
    .unwrap());
    assert_family4_equal(&loaded, &snap);

    // Scalar reversion back to Default emits `DomainRowChange::Delete` and removes scalar row cleanly.
    let mut reverted = loaded.clone();
    reverted.war_state.next_negotiation_id = 0;
    reverted.fw_wars.clear();
    let mut scalar_diffs = Vec::new();
    collect_corp_crimewatch_domain_row_diffs(&loaded, &reverted, &mut scalar_diffs);
    assert_eq!(scalar_diffs.len(), 2);
    assert!(scalar_diffs.iter().all(|d| matches!(d, DomainRowChange::Delete { .. })));

    // Mutate all 3 domains in memory and verify `rollback_corp_crimewatch` restores all 36 fields.
    loaded.corp_member_limits.insert(98_000_001, 999);
    loaded.wh_polarizations.clear();
    loaded.fw_wars.clear();
    loaded.war_state.next_negotiation_id = 0;
    loaded.combat_crimewatch_state.player_bounty_pool.clear();
    loaded.sov_runtime.sov_upgrade_combat_npcs.clear();
    loaded.insurgency_runtime.active_state_notified.clear();

    rollback_corp_crimewatch(&conn, &mut loaded).expect("rollback_corp_crimewatch");
    assert_family4_equal(&loaded, &snap);
}

#[test]
fn corp_crimewatch_legacy_kv_and_blob_migration_preserves_newer_domain_rows_and_drops_table() {
    use crate::sqlite_store::load::load_snapshot_from_sqlite;

    let path = temp_corp_crimewatch_db_path("legacy_migration");
    let _ = std::fs::remove_file(&path);
    let conn = Connection::open(&path).expect("open");
    apply_migrations(&conn).expect("migrate");

    conn.execute_batch(LEGACY_D6_CORP_CRIMEWATCH_SQL)
        .expect("create legacy d6_corp_crimewatch_kv");

    let mut legacy_snap = WorldStateSnapshot::default();
    populate_all_family4_fields(&mut legacy_snap);

    let legacy_corp = CorpGovernanceDomainPayload {
        corp_governance: legacy_snap.corp_governance.clone(),
        employment_history: legacy_snap.employment_history.clone(),
        corp_hangar_divisions: legacy_snap.corp_hangar_divisions.clone(),
        corp_alliance_history: legacy_snap.corp_alliance_history.clone(),
        corp_container_logs: legacy_snap.corp_container_logs.clone(),
        corp_medals: legacy_snap.corp_medals.clone(),
        corp_issued_medals: legacy_snap.corp_issued_medals.clone(),
        corp_member_limits: legacy_snap.corp_member_limits.clone(),
        corp_titles: legacy_snap.corp_titles.clone(),
        corp_member_titles: legacy_snap.corp_member_titles.clone(),
        corp_role_assignments: legacy_snap.corp_role_assignments.clone(),
        corp_role_history: legacy_snap.corp_role_history.clone(),
        corp_shareholders: legacy_snap.corp_shareholders.clone(),
        char_medals: legacy_snap.char_medals.clone(),
        char_scoped_roles: legacy_snap.char_scoped_roles.clone(),
        corp_vote_cases: legacy_snap.corp_vote_cases.clone(),
        corp_vote_ballots: legacy_snap.corp_vote_ballots.clone(),
    };
    let legacy_cw = CrimewatchDomainPayload {
        crimewatch_states: legacy_snap.crimewatch_states.clone(),
        crimewatch_aggressions: legacy_snap.crimewatch_aggressions.clone(),
        combat_crimewatch_state: legacy_snap.combat_crimewatch_state.clone(),
        wh_polarizations: legacy_snap.wh_polarizations.clone(),
        zarzakh_gate_locks: legacy_snap.zarzakh_gate_locks.clone(),
        structure_docking_acl_denied: legacy_snap
            .structure_docking_acl_denied
            .iter()
            .map(|(&k, set)| (k, set.iter().copied().collect()))
            .collect(),
    };
    let legacy_war = WarfareDomainPayload {
        alliance_war_state: legacy_snap.alliance_war_state.clone(),
        war_state: legacy_snap.war_state.clone(),
        fw_runtime: legacy_snap.fw_runtime.clone(),
        sov_runtime: legacy_snap.sov_runtime.clone(),
        insurgency_runtime: legacy_snap.insurgency_runtime.clone(),
        sov_prospecting_levels: legacy_snap.sov_prospecting_levels.clone(),
        sov_campaign_scores: legacy_snap.sov_campaign_scores.clone(),
        fw_character_stats: legacy_snap.fw_character_stats.clone(),
        fw_corp_enlisted_on: legacy_snap.fw_corp_enlisted_on.clone(),
        fw_system_occupations: legacy_snap.fw_system_occupations.clone(),
        fw_wars: legacy_snap.fw_wars.clone(),
        war_lifecycle_meta: legacy_snap.war_lifecycle_meta.clone(),
        alliance_lifecycle_meta: legacy_snap.alliance_lifecycle_meta.clone(),
    };

    for (key, payload) in [
        (KEY_CORP_GOV, serde_json::to_string(&legacy_corp).expect("corp json")),
        (KEY_CRIMEWATCH, serde_json::to_string(&legacy_cw).expect("cw json")),
        (KEY_WARFARE, serde_json::to_string(&legacy_war).expect("war json")),
    ] {
        conn.execute(
            "INSERT INTO d6_corp_crimewatch_kv (store_key, payload_json) VALUES (?1, ?2)",
            params![key, payload],
        )
        .expect("seed legacy row");
    }

    // Seed stale duplicate rows in `world_kv_blobs` that must be drained without overwriting newer rows.
    let stale_logs_json = serde_json::to_string(&vec![(
        98_000_001_i64,
        vec![CorpContainerLogEntry {
            action: "stale_blob_log".into(),
            ..Default::default()
        }],
    )])
    .expect("stale logs");
    let stale_pol_json =
        serde_json::to_string(&vec![((90_000_001_i64, 31_000_001_i64), 1111_u64)])
            .expect("stale polarizations");
    conn.execute(
        "INSERT INTO world_kv_blobs (blob_key, blob_json) VALUES ('corp_container_logs', ?1), ('wh_polarizations', ?2)",
        params![stale_logs_json, stale_pol_json],
    )
    .expect("seed duplicate world_kv_blobs");

    // Pre-seed newer `domain_rows` overrides across all 3 domains so migration must not overwrite them.
    let newer_cw_state = PersistedCrimeWatchState {
        character_id: 90_000_001,
        weapon_timer_until_unix: 99_999,
        ..Default::default()
    };
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'corp_member_limits/98000001', '500')",
        params![DOMAIN_CORP_GOVERNANCE],
    )
    .expect("seed newer corp_governance limit");
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'crimewatch_states/90000001', ?2)",
        params![DOMAIN_CRIMEWATCH, serde_json::to_string(&newer_cw_state).unwrap()],
    )
    .expect("seed newer crimewatch state");
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'war_state.next_negotiation_id', '42')",
        params![DOMAIN_WARFARE],
    )
    .expect("seed newer warfare scalar");

    let (migrated, _fp) = load_snapshot_from_sqlite(&conn).expect("load + migrate");

    // Retired legacy table and duplicate `world_kv_blobs` keys must be removed after commit.
    let legacy_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'd6_corp_crimewatch_kv'",
            [],
            |r| r.get(0),
        )
        .expect("check dropped legacy table");
    assert_eq!(legacy_exists, 0);
    let dup_blobs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM world_kv_blobs WHERE blob_key IN ('corp_container_logs', 'wh_polarizations')",
            [],
            |r| r.get(0),
        )
        .expect("check cleaned world_kv_blobs");
    assert_eq!(dup_blobs, 0);

    // Newer `domain_rows` values win over legacy rows, while non-conflicting legacy rows migrate cleanly.
    assert_eq!(migrated.corp_member_limits.get(&98_000_001), Some(&500));
    assert_eq!(
        migrated
            .crimewatch_states
            .get(&90_000_001)
            .map(|s| s.weapon_timer_until_unix),
        Some(99_999)
    );
    assert_eq!(migrated.war_state.next_negotiation_id, 42);
    // `corp_container_logs` & `wh_polarizations` keep legacy `d6_corp_crimewatch_kv` over stale `world_kv_blobs`.
    assert_eq!(migrated.corp_container_logs, legacy_snap.corp_container_logs);
    assert_eq!(migrated.wh_polarizations, legacy_snap.wh_polarizations);
    assert_eq!(migrated.corp_hangar_divisions, legacy_snap.corp_hangar_divisions);
    assert_eq!(migrated.structure_docking_acl_denied, legacy_snap.structure_docking_acl_denied);
    assert_eq!(migrated.fw_wars, legacy_snap.fw_wars);

    // Migrated rows now live directly in `domain_rows` and reload with exact parity.
    let migrated_limit: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'corp_member_limits/98000001'",
            [DOMAIN_CORP_GOVERNANCE],
            |r| r.get(0),
        )
        .expect("migrated domain_rows entry");
    assert_eq!(migrated_limit, "500");

    let (reloaded, _) = load_snapshot_from_sqlite(&conn).expect("reload parity");
    assert_family4_equal(&reloaded, &migrated);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn corp_crimewatch_tx_writes_single_row_zero_cold_clones_and_rejects_non_finite() {
    use crate::persistence::{
        reset_thread_cold_fp_clones_for_test, thread_cold_fp_clones_for_test, WorldRepository,
    };
    use crate::sqlite_store::domain_rows::{
        apply_domain_row_changes, validate_domain_row_changes, DomainRowChange,
    };
    use crate::sqlite_store::sync::replace_sqlite_from_snapshot;
    use crate::tx_engine::{ChangeSet, TxError, TxSqlExecutor};

    let path = temp_corp_crimewatch_db_path("tx_row_granularity");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open_sqlite");

    repo.tx("seed_corp_crimewatch", |wtx| -> Result<(), TxError> {
        wtx.snap.corp_member_limits.insert(98_000_001, 50);
        wtx.snap.corp_member_limits.insert(98_000_002, 60);
        wtx.snap.crimewatch_states.insert(
            90_000_001,
            PersistedCrimeWatchState {
                character_id: 90_000_001,
                weapon_timer_until_unix: 100,
                ..Default::default()
            },
        );
        wtx.snap
            .combat_crimewatch_state
            .player_bounty_pool
            .insert(90_000_001, 500.0);
        wtx.snap.wh_polarizations.insert((90_000_001, 31_000_001), 7);
        wtx.snap.sov_campaign_scores.insert(
            7001,
            SovCampaignScoreRecord {
                structure_id: 7001,
                attackers_score: 0.5,
                defender_score: 0.5,
            },
        );
        Ok(())
    })
    .expect("seed tx");

    // Mutating 1 entry inside `repo.tx` triggers 0 cold fingerprint clones.
    reset_thread_cold_fp_clones_for_test();
    repo.tx("update_one_corp_limit", |wtx| -> Result<(), TxError> {
        wtx.snap.corp_member_limits.insert(98_000_001, 125);
        Ok(())
    })
    .expect("corp limit update tx");
    assert_eq!(thread_cold_fp_clones_for_test(), 0);

    reset_thread_cold_fp_clones_for_test();
    repo.tx("update_one_bounty_pool", |wtx| -> Result<(), TxError> {
        wtx.snap
            .combat_crimewatch_state
            .player_bounty_pool
            .insert(90_000_001, 750.0);
        Ok(())
    })
    .expect("crimewatch bounty update tx");
    assert_eq!(thread_cold_fp_clones_for_test(), 0);

    // Direct `ChangeSet::diff` + `apply_domain_row_changes` writes strictly 1 SQLite row.
    let before_snap = repo.with_snapshot(Clone::clone);
    let mut after_snap = before_snap.clone();
    after_snap
        .combat_crimewatch_state
        .player_bounty_pool
        .insert(90_000_001, 1_250.25);
    let changes = ChangeSet::diff(2, &before_snap, &after_snap);
    assert_eq!(changes.domain_rows.len(), 1);
    assert!(!changes.cold_domains.d6_corp_crimewatch);
    assert!(changes.cold_domains.crimewatch_changed);
    assert!(matches!(
        &changes.domain_rows[0],
        DomainRowChange::Upsert { domain, key, .. }
            if *domain == DOMAIN_CRIMEWATCH
                && key == "combat_crimewatch_state.player_bounty_pool/90000001"
    ));

    let mut mem_conn = Connection::open_in_memory().expect("open mem");
    apply_migrations(&mem_conn).expect("migrate mem");
    replace_sqlite_from_snapshot(&mut mem_conn, &before_snap).expect("seed mem");
    let before_changes = mem_conn.total_changes();
    {
        let sql_tx = mem_conn.transaction().expect("begin sql tx");
        let mut exec = TxSqlExecutor::new(&sql_tx, crate::tx_engine::TxFaultPlan::None);
        apply_domain_row_changes(&mut exec, &changes.domain_rows).expect("apply 1 row");
        sql_tx.commit().expect("commit 1 row");
    }
    assert_eq!(
        mem_conn.total_changes() - before_changes,
        1,
        "mutating 1 Family 4 entry must write strictly 1 SQLite row"
    );

    // Non-finite floats (NaN / Inf) fail validation and rollback tx naming exact domain & key.
    for bad_val in [f64::NAN, f64::INFINITY] {
        let mut bad_cw = before_snap.clone();
        bad_cw
            .combat_crimewatch_state
            .player_bounty_pool
            .insert(90_000_001, bad_val);
        let cw_diff = ChangeSet::diff(3, &before_snap, &bad_cw);
        let err = validate_domain_row_changes(&cw_diff.domain_rows, 1024 * 1024)
            .expect_err("NaN/Inf in player_bounty_pool must fail validation");
        assert!(matches!(err, TxError::NonFinite { .. }), "got: {err}");
        let msg = err.to_string();
        assert!(msg.contains(DOMAIN_CRIMEWATCH), "{msg}");
        assert!(
            msg.contains("combat_crimewatch_state.player_bounty_pool/90000001"),
            "{msg}"
        );

        let tx_err: Result<(), TxError> = repo.tx("reject_bad_warfare_float", |wtx| {
            wtx.snap
                .sov_campaign_scores
                .get_mut(&7001)
                .unwrap()
                .attackers_score = bad_val;
            Ok(())
        });
        let tx_msg = tx_err.expect_err("repo.tx must reject NaN/Inf").to_string();
        assert!(tx_msg.contains(DOMAIN_WARFARE), "{tx_msg}");
        assert!(tx_msg.contains("sov_campaign_scores/7001"), "{tx_msg}");
    }
    assert_eq!(
        repo.with_snapshot(|s| s.sov_campaign_scores[&7001].attackers_score),
        0.5
    );

    drop(repo);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corp_crimewatch_fail_closed_load_and_rollback_preserve_snapshot_on_corrupt_row() {
    use crate::sqlite_store::quarantine::LoadContext;

    let conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();

    // Seed valid rows for corp_governance and warfare, plus 1 corrupt row for crimewatch.
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'corp_member_limits/98000001', '77'), (?2, 'crimewatch_states/90000001', '{corrupt'), (?3, 'war_state.next_negotiation_id', '99')",
        params![DOMAIN_CORP_GOVERNANCE, DOMAIN_CRIMEWATCH, DOMAIN_WARFARE],
    )
    .unwrap();

    let mut snap = WorldStateSnapshot::default();
    snap.corp_member_limits.insert(98_000_001, 10);
    snap.wh_polarizations.insert((90_000_001, 31_000_001), 4242);
    snap.war_state.next_negotiation_id = 5;

    let err = load_corp_crimewatch_domain_rows(&conn, &mut snap, &mut LoadContext::new(false))
        .unwrap_err();
    assert!(err.contains(DOMAIN_CRIMEWATCH), "unexpected err: {err}");
    assert_eq!(snap.corp_member_limits.get(&98_000_001), Some(&10));
    assert_eq!(snap.wh_polarizations.get(&(90_000_001, 31_000_001)), Some(&4242));
    assert_eq!(snap.war_state.next_negotiation_id, 5);

    let failures_before = crate::persist_failures_total();
    let rb_err = rollback_corp_crimewatch(&conn, &mut snap).unwrap_err();
    assert!(rb_err.contains(DOMAIN_CRIMEWATCH), "unexpected err: {rb_err}");
    assert!(crate::persist_failures_total() >= failures_before + 1);
    assert_eq!(snap.corp_member_limits.get(&98_000_001), Some(&10));
    assert_eq!(snap.wh_polarizations.get(&(90_000_001, 31_000_001)), Some(&4242));
    assert_eq!(snap.war_state.next_negotiation_id, 5);

    // Clean existing `domain_rows` restores dirty snapshot back to committed rows / defaults.
    conn.execute("DELETE FROM domain_rows", []).unwrap();
    rollback_corp_crimewatch(&conn, &mut snap).expect("clean rollback to empty domain_rows");
    assert!(snap.corp_member_limits.is_empty());
    assert!(snap.wh_polarizations.is_empty());
    assert_eq!(snap.war_state.next_negotiation_id, 0);
}
