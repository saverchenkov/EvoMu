//! Unit and mutation-killing tests for `sqlite_store::corp_crimewatch_store`.

use super::*;
use crate::sqlite_store::checked_json::{canonical_fp, FnvHasher, FnvSerError, FnvSerializer};
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
    snap.combat_crimewatch_state
        .modified_system_securities
        .insert(30_000_142, 0.35);
    snap.combat_crimewatch_state
        .ess_reserve_unlocked_until
        .insert(30_000_142, 1_700_009_000);
    snap.combat_crimewatch_state
        .ess_reserve_bank_thefts
        .insert(30_000_142, vec![(90_000_001, 250_000.0, 1_700_008_000)]);
    snap.combat_crimewatch_state
        .ess_main_bank_thefts
        .insert(30_000_142, vec![(90_000_002, 500_000.0, 1_700_008_500)]);
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

    // Mutate all 3 domains in memory and verify `load_corp_crimewatch_domain_rows` restores all 36 fields.
    loaded.corp_member_limits.insert(98_000_001, 999);
    loaded.wh_polarizations.clear();
    loaded.fw_wars.clear();
    loaded.war_state.next_negotiation_id = 0;
    loaded.combat_crimewatch_state.player_bounty_pool.clear();
    loaded.sov_runtime.sov_upgrade_combat_npcs.clear();
    loaded.insurgency_runtime.active_state_notified.clear();

    load_corp_crimewatch_domain_rows(
        &conn,
        &mut loaded,
        &mut crate::sqlite_store::quarantine::LoadContext::new(false),
    )
    .expect("load_corp_crimewatch_domain_rows");
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
        corp_governance: legacy_snap.corp_governance.as_inner().clone(),
        employment_history: legacy_snap.employment_history.as_inner().clone(),
        corp_hangar_divisions: legacy_snap.corp_hangar_divisions.as_inner().clone(),
        corp_alliance_history: legacy_snap.corp_alliance_history.as_inner().clone(),
        corp_container_logs: legacy_snap.corp_container_logs.as_inner().clone(),
        corp_medals: legacy_snap.corp_medals.as_inner().clone(),
        corp_issued_medals: legacy_snap.corp_issued_medals.as_inner().clone(),
        corp_member_limits: legacy_snap.corp_member_limits.as_inner().clone(),
        corp_titles: legacy_snap.corp_titles.as_inner().clone(),
        corp_member_titles: legacy_snap.corp_member_titles.as_inner().clone(),
        corp_role_assignments: legacy_snap.corp_role_assignments.as_inner().clone(),
        corp_role_history: legacy_snap.corp_role_history.as_inner().clone(),
        corp_shareholders: legacy_snap.corp_shareholders.as_inner().clone(),
        char_medals: legacy_snap.char_medals.as_inner().clone(),
        char_scoped_roles: legacy_snap.char_scoped_roles.as_inner().clone(),
        corp_vote_cases: legacy_snap.corp_vote_cases.as_inner().clone(),
        corp_vote_ballots: legacy_snap.corp_vote_ballots.as_inner().clone(),
    };
    let legacy_cw = CrimewatchDomainPayload {
        crimewatch_states: legacy_snap.crimewatch_states.as_inner().clone(),
        crimewatch_aggressions: legacy_snap.crimewatch_aggressions.as_inner().clone(),
        combat_crimewatch_state: legacy_snap.combat_crimewatch_state.as_inner().clone(),
        wh_polarizations: legacy_snap.wh_polarizations.as_inner().clone(),
        zarzakh_gate_locks: legacy_snap.zarzakh_gate_locks.as_inner().clone(),
        structure_docking_acl_denied: legacy_snap
            .structure_docking_acl_denied
            .iter()
            .map(|(&k, set)| (k, set.iter().copied().collect()))
            .collect(),
    };
    let legacy_war = WarfareDomainPayload {
        alliance_war_state: legacy_snap.alliance_war_state.as_inner().clone(),
        war_state: legacy_snap.war_state.as_inner().clone(),
        fw_runtime: legacy_snap.fw_runtime.as_inner().clone(),
        sov_runtime: legacy_snap.sov_runtime.as_inner().clone(),
        insurgency_runtime: legacy_snap.insurgency_runtime.as_inner().clone(),
        sov_prospecting_levels: legacy_snap.sov_prospecting_levels.as_inner().clone(),
        sov_campaign_scores: legacy_snap.sov_campaign_scores.as_inner().clone(),
        fw_character_stats: legacy_snap.fw_character_stats.as_inner().clone(),
        fw_corp_enlisted_on: legacy_snap.fw_corp_enlisted_on.as_inner().clone(),
        fw_system_occupations: legacy_snap.fw_system_occupations.as_inner().clone(),
        fw_wars: legacy_snap.fw_wars.as_inner().clone(),
        war_lifecycle_meta: legacy_snap.war_lifecycle_meta.as_inner().clone(),
        alliance_lifecycle_meta: legacy_snap.alliance_lifecycle_meta.as_inner().clone(),
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
    conn.execute_batch(crate::sqlite_store::industry_market_pve_store::LEGACY_WORLD_KV_BLOBS_SQL)
        .expect("create legacy world_kv_blobs");
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

    let migrated = load_snapshot_from_sqlite(&conn).expect("load + migrate");

    // Retired legacy table and duplicate `world_kv_blobs` keys must be removed after commit.
    let legacy_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'd6_corp_crimewatch_kv'",
            [],
            |r| r.get(0),
        )
        .expect("check dropped legacy table");
    assert_eq!(legacy_exists, 0);
    let legacy_blobs_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'world_kv_blobs'",
            [],
            |r| r.get(0),
        )
        .expect("check dropped legacy world_kv_blobs");
    assert_eq!(legacy_blobs_exists, 0);

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

    let reloaded = load_snapshot_from_sqlite(&conn).expect("reload parity");
    assert_family4_equal(&reloaded, &migrated);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn corp_crimewatch_tx_writes_single_row_zero_cold_clones_and_rejects_non_finite() {
    use crate::persistence::{
        reset_thread_cold_domain_syncs_for_test, thread_cold_domain_syncs_for_test, WorldRepository,
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

    // Mutating 1 entry inside `repo.tx` triggers 0 cold domain syncs.
    reset_thread_cold_domain_syncs_for_test();
    repo.tx("update_one_corp_limit", |wtx| -> Result<(), TxError> {
        wtx.snap.corp_member_limits.insert(98_000_001, 125);
        Ok(())
    })
    .expect("corp limit update tx");
    assert_eq!(thread_cold_domain_syncs_for_test(), 0);

    reset_thread_cold_domain_syncs_for_test();
    repo.tx("update_one_bounty_pool", |wtx| -> Result<(), TxError> {
        wtx.snap
            .combat_crimewatch_state
            .player_bounty_pool
            .insert(90_000_001, 750.0);
        Ok(())
    })
    .expect("crimewatch bounty update tx");
    assert_eq!(thread_cold_domain_syncs_for_test(), 0);

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
fn corp_crimewatch_fail_closed_load_preserves_snapshot_on_corrupt_row() {
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
}

#[test]
fn corp_crimewatch_all_substruct_fields_survive_tx_commit_and_sqlite_reopen() {
    use crate::durable_domain_types::*;
    use crate::persistence::WorldRepository;
    use crate::tx_engine::TxError;
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

    let path = temp_corp_crimewatch_db_path("all_substruct_fields_roundtrip");
    let _ = std::fs::remove_file(&path);
    let repo = WorldRepository::open_sqlite(&path).expect("open_sqlite");

    let expected_combat = PersistedCombatCrimewatchState {
        sec_status_transactions: HashMap::from([(
            90_000_001,
            vec![PersistedSecStatusTransaction {
                timestamp_unix: 1_700_000_100,
                old_sec: 1.5,
                new_sec: 1.25,
                delta: -0.25,
                reason: "pod_aggression".to_string(),
                solar_system_id: 30_000_142,
                event_type_id: 7,
                other_owner_id: Some(90_000_002),
                other_type_id: Some(587),
            }],
        )]),
        safety_levels: HashMap::from([(90_000_001, 2)]),
        drone_settings: HashMap::from([(90_000_001, (true, false))]),
        limited_engagements: HashMap::from([(
            90_000_001,
            HashMap::from([(90_000_002, 1_700_000_900)]),
        )]),
        npc_timers: HashMap::from([(90_000_001, 1_700_000_300)]),
        disapproval_flags: HashMap::from([(90_000_001, 1_700_000_400)]),
        duel_challenges: HashMap::from([((90_000_001, 90_000_002), 1_700_000_500)]),
        corp_aggression_settings: HashMap::from([(98_000_001, true)]),
        player_bounty_pool: HashMap::from([(90_000_001, 125_000.5)]),
        bounty_contributions: HashMap::from([(90_000_001, vec![(90_000_002, 25_000.25)])]),
        bounty_hunter_claimed: HashMap::from([(90_000_003, 50_000.0)]),
        kill_right_expires_unix: HashMap::from([(8001, 1_700_008_000)]),
        kill_right_restricted_to: HashMap::from([(8001, 98_000_001)]),
        detailed_killmails: HashMap::from([(
            7001,
            DetailedKillmail {
                kill_id: 7001,
                kill_time_unix: 1_700_000_600,
                solar_system_id: 30_000_142,
                victim_character_id: 90_000_001,
                victim_corporation_id: 98_000_001,
                victim_alliance_id: Some(99_000_001),
                victim_ship_type_id: 587,
                victim_damage_taken: 1_250.5,
                victim_position: [10.0, 20.0, 30.0],
                attackers: vec![KillmailAttackerEntry {
                    character_id: 90_000_002,
                    corporation_id: 98_000_002,
                    alliance_id: None,
                    ship_type_id: 588,
                    weapon_type_id: 3041,
                    damage_done: 1_250.5,
                    final_blow: true,
                    security_status: 2.5,
                }],
                dropped_items: vec![KillmailItemEntry {
                    type_id: 587,
                    flag: 0,
                    qty_dropped: 1,
                    qty_destroyed: 0,
                    singleton: false,
                }],
                destroyed_items: vec![KillmailItemEntry {
                    type_id: 3041,
                    flag: 27,
                    qty_dropped: 0,
                    qty_destroyed: 2,
                    singleton: false,
                }],
                total_value_isk: 450_000.0,
                killmail_hash: "km-hash-7001".to_string(),
            },
        )]),
        pending_bounty_ticks: HashMap::from([((30_000_142, 90_000_001), 15_000.0)]),
        bounty_tick_next_flush_unix: HashMap::from([((30_000_142, 90_000_001), 1_700_001_200)]),
        pending_bounty_tick_started_unix: HashMap::from([(
            (30_000_142, 90_000_001),
            1_700_000_000,
        )]),
        modified_system_securities: HashMap::from([(30_000_142, 0.35)]),
        ess_reserve_unlocked_until: HashMap::from([(30_000_142, 1_700_009_000)]),
        ess_reserve_bank_thefts: HashMap::from([(
            30_000_142,
            vec![(90_000_001, 250_000.0, 1_700_008_000)],
        )]),
        ess_main_bank_thefts: HashMap::from([(
            30_000_142,
            vec![(90_000_002, 500_000.0, 1_700_008_500)],
        )]),
        extended_runtime_json: "{\"bounty_flush\":1}".to_string(),
    };

    let expected_alliance_war = PersistedAllianceWarState {
        alliances: HashMap::from([(
            99_000_001,
            PersistedAllianceMeta {
                description: "Executor alliance".to_string(),
                url: "https://example.invalid".to_string(),
                logo: (1, 2, 3, 4),
                emblem_entitlements: true,
                dictatorial: false,
                executor_votes: HashMap::from([(98_000_001, 98_000_001)]),
                ..Default::default()
            },
        )]),
        project_meta: HashMap::from([(401, ("Mineral Drive".to_string(), true, false))]),
        project_details: HashMap::from([(
            401,
            PersistedProjectDetails {
                wallet_division: 2,
                status: "active".to_string(),
                creator_id: 90_000_001,
                created_unix: 1_700_000_050,
                batch_size: 100,
                initial_escrow: IskCents(50_000),
                ..Default::default()
            },
        )]),
    };

    let expected_war = PersistedWarState {
        wars: HashMap::from([(
            500,
            PersistedWarMeta {
                war_hq_id: Some(60_003_760),
                open_for_allies: true,
                finish_at_unix: Some(1_800_000_000),
            },
        )]),
        negotiations: vec![PersistedWarNegotiation {
            id: 11,
            war_id: 500,
            kind: PersistedNegKind::Ally,
            from_corp: 98_000_009,
            isk: IskCents(25_000),
            message: "Offering defense".to_string(),
            status: "accepted".to_string(),
            effective_unix: Some(1_700_050_000),
        }],
        next_negotiation_id: 12,
        mutual_invites: HashMap::from([((98_000_001, 98_000_002), 1_700_060_000)]),
        blocked_mutual_invites: HashMap::from([(98_000_003, true)]),
        peace_treaties: vec![PersistedPeaceTreaty {
            war_id: 499,
            corp_a: 98_000_001,
            corp_b: 98_000_004,
            expiry_unix: 1_900_000_000,
        }],
        ally_expires_unix: HashMap::from([((500, 98_000_009), 1_750_000_000)]),
    };

    let expected_fw = PersistedFwState {
        system_victory_points: HashMap::from([(30_000_142, 420)]),
        system_advantage: HashMap::from([(30_000_142, (0.35, 0.15))]),
        battlefield_instances: vec![PersistedBattlefieldRecord {
            instance_id: 101,
            solar_system_id: 30_000_142,
            faction_id: 500_001,
            name: "Caldari Battlefield".to_string(),
            capture_progress: 0.5,
            completed: false,
        }],
        battlefield_zones: HashMap::from([(101, [1, 2, 3])]),
        battlefield_participants: HashMap::from([(101, vec![90_000_001])]),
        battlefield_pilot_seconds: HashMap::from([(101, HashMap::from([(90_000_001, 45)]))]),
        battlefield_waves: HashMap::from([(101, 2)]),
        battlefield_respawn_unix: HashMap::from([(30_000_142, 1_700_002_000)]),
        fw_complexes: HashMap::from([(201, (30_000_142, "novice".to_string(), 500_001, 0.25, false))]),
        ihub_lp_donations: HashMap::from([(30_000_142, 5_000)]),
        ihub_tiers: HashMap::from([(30_000_142, 3)]),
        ihub_vulnerable: HashMap::from([(30_000_142, true)]),
        ihub_vulnerable_since_unix: HashMap::from([(30_000_142, 1_700_000_200)]),
        last_ihub_attacked_unix: HashMap::from([(30_000_142, 1_700_000_250)]),
        pending_downtime_captures: HashMap::from([(30_000_143, 500_004)]),
        corp_war_factions: HashMap::from([(98_000_001, 500_001)]),
        complex_occupants: HashMap::from([(201, vec![(90_000_001, 500_001)])]),
        supply_sites: HashMap::from([(301, (30_000_142, "depot".to_string(), 500_001))]),
        direct_enlisted_chars: HashMap::from([(90_000_001, 500_001)]),
        complex_last_step_unix: HashMap::from([(201, 1_700_000_300)]),
        complex_capturing_faction: HashMap::from([(201, 500_001)]),
        complex_seconds_elapsed: HashMap::from([(201, 120)]),
        complex_guards_cleared: HashMap::from([(201, true)]),
        complex_tier_cooldowns: HashMap::from([("30000142:novice".to_string(), 1_700_000_900)]),
        battlefield_npc_waves: HashMap::from([(101, vec![10_001, 10_002])]),
        stat_events: vec![PersistedFwStatEvent {
            timestamp_unix: 1_700_000_400,
            character_id: 90_000_001,
            corporation_id: 98_000_001,
            alliance_id: Some(99_000_001),
            faction_id: 500_001,
            kills: 3,
            victory_points: 150,
        }],
        historical_char_totals: HashMap::from([(90_000_001, (3, 150))]),
        historical_corp_totals: HashMap::from([(98_000_001, (3, 150))]),
        historical_fac_totals: HashMap::from([(500_001, (3, 150))]),
        historical_ally_totals: HashMap::from([(99_000_001, (3, 150))]),
    };

    let expected_sov = PersistedSovState {
        fuel_access_groups: HashMap::from([(6001, 42)]),
        hacked_hubs: HashMap::from([(6001, 1_700_001_000)]),
        skyhook_reagents: HashMap::from([(6002, (81_143, 500))]),
        skyhook_hackable: HashMap::from([(6002, true)]),
        skyhook_active: BTreeMap::from([(6002, true)]),
        skyhook_planets: HashMap::from([(6002, 40_009_082)]),
        skyhook_theft_windows: HashMap::from([(6002, (1_700_000_100, 1_700_003_700))]),
        sov_hubs: HashMap::from([(
            6001,
            PersistedSovHubData {
                hub_id: 6001,
                upgrades: vec![PersistedHubUpgrade {
                    type_id: 81_561,
                    online: true,
                    low_power: false,
                }],
                fuel: BTreeMap::from([(81_143, 250)]),
                workforce: PersistedWorkforceMode::Import {
                    sources: vec![30_000_142],
                },
                last_updated_unix: 1_700_000_500,
            },
        )]),
        mercenary_dens: HashMap::from([(
            6003,
            PersistedMercenaryDen {
                den_id: 6003,
                type_id: 85_230,
                owner_char_id: 90_000_001,
                skyhook_id: 6002,
                solar_system_id: 30_000_142,
                planet_id: 40_009_082,
                enabled: true,
                cargo_extraction_enabled: false,
                development_level: 2,
                anarchy_level: 1,
                infomorphs: 15,
                simulated_at_unix: 1_700_000_600,
                next_activity_generation_unix: 1_700_004_200,
            },
        )]),
        mercenary_activities: BTreeMap::from([(
            "act-1".to_string(),
            PersistedMercenaryActivity {
                uuid: vec![1, 2, 3, 4],
                den_id: 6003,
                solar_system_id: 30_000_142,
                dungeon_id: 1234,
                started: true,
                started_by: 90_000_001,
                expiry_unix: 1_700_005_000,
                development_impact: 1,
                anarchy_impact: -1,
                infomorph_bonus: 5,
            },
        )]),
        next_mercenary_activity_seq: 9,
        skyhook_reagent_bays: HashMap::from([(6002, (81_143, 100, 50, 1_700_000_700))]),
        mercenary_den_reinforcement: HashMap::from([(6003, (1_700_000_800, 1_700_004_400, 1))]),
        mercenary_den_tax_pct: HashMap::from([(6003, 10)]),
        command_nodes: HashMap::from([(7001, (6001, 30_000_142, 99_000_001))]),
        sov_npc_kills: HashMap::from([(30_000_142, 75)]),
        sov_mined_units: HashMap::from([(30_000_142, 12_500)]),
        adm_vulnerability_windows: HashMap::from([(6001, (1_700_001_000, 1_700_011_800))]),
        skyhook_maturity_rem: HashMap::from([(6002, (1_700_000_000, (1u128 << 80) + 42))]),
        mercenary_den_evo_rem: HashMap::from([(6003, 600)]),
        skyhook_last_harvest_unix: HashMap::from([(6002, 1_700_000_950)]),
        skyhook_silo_links: HashMap::from([(6002, (6004, 98_000_001, 1_700_000_999, 0.75))]),
        entosis_capture_progress: HashMap::from([(7001, (90_000_001, 98_000_001, 6001, 0.65, 1_700_001_100))]),
        tenebrex_spool_ready_unix: HashMap::from([(6005, 1_700_001_300)]),
        tenebrex_spool_notified: HashMap::from([(6005, true)]),
        sov_claimed_since_unix: HashMap::from([(30_000_142, 1_690_000_000)]),
        mto_site_balls: HashMap::from([(7002, "mto-beacon".to_string())]),
        command_node_scores: HashMap::from([(7001, vec![(99_000_001, 40.5)])]),
        sov_upgrade_combat_npcs: BTreeSet::from([80_001, 80_002]),
        sov_dev_decay_last_unix: HashMap::from([(30_000_142, 1_700_001_400)]),
        command_node_regen_last_unix: HashMap::from([(7001, 1_700_001_500)]),
    };

    let expected_insurgency = PersistedInsurgencyState {
        campaigns: HashMap::from([(
            901,
            PersistedInsurgencyCampaign {
                campaign_id: 901,
                pirate_faction_id: 500_010,
                target_faction_id: 500_001,
                fob_solar_system_id: 30_000_142,
                systems: vec![30_000_142, 30_000_143],
                created_at_unix: 1_700_000_000,
                forecasting_end_unix: 1_700_003_600,
                active_end_unix: 1_700_086_400,
                winddown_end_unix: 1_700_090_000,
                corruption_points: HashMap::from([(30_000_142, 300)]),
                suppression_points: HashMap::from([(30_000_142, 150)]),
            },
        )]),
        insurgency_instances: vec![PersistedInsurgencySiteRecord {
            instance_id: 902,
            solar_system_id: 30_000_142,
            site_type: "ambush".to_string(),
            name: "Pirate Hideout".to_string(),
            corruption_reward: 12.5,
            completed: false,
        }],
        ice_heist_instances: vec![PersistedIceHeistRecord {
            instance_id: 903,
            solar_system_id: 30_000_143,
            pirate_tankers_saved: 2,
            empire_tankers_saved: 1,
            completed: true,
        }],
        pirate_ambition_modifier: HashMap::from([(901, 2)]),
        empire_ambition_modifier: HashMap::from([(901, 1)]),
        fob_armor_reinforced_until: HashMap::from([(901, 1_700_040_000)]),
        campaign_contributions: HashMap::from([(901, vec![(90_000_001, 100, 50)])]),
        campaign_outcomes: HashMap::from([(900, "pirate_victory".to_string())]),
        surrender_ihub_vulnerable_unix: HashMap::from([(30_000_142, 1_700_095_000)]),
        active_state_notified: HashSet::from([901, 902]),
    };

    repo.tx("seed_all_substruct_fields", |wtx| -> Result<(), TxError> {
        wtx.snap.combat_crimewatch_state = expected_combat.clone().into();
        wtx.snap.alliance_war_state = expected_alliance_war.clone().into();
        wtx.snap.war_state = expected_war.clone().into();
        wtx.snap.fw_runtime = expected_fw.clone().into();
        wtx.snap.sov_runtime = expected_sov.clone().into();
        wtx.snap.insurgency_runtime = expected_insurgency.clone().into();
        Ok(())
    })
    .expect("commit all family4 substruct fields");
    drop(repo);

    let reopened = WorldRepository::open_sqlite(&path).expect("reopen after substruct commit");
    reopened.with_snapshot(|snap| {
        assert_eq!(&*snap.combat_crimewatch_state, &expected_combat);
        assert_eq!(&*snap.alliance_war_state, &expected_alliance_war);
        assert_eq!(&*snap.war_state, &expected_war);
        assert_eq!(&*snap.fw_runtime, &expected_fw);
        assert_eq!(&*snap.sov_runtime, &expected_sov);
        assert_eq!(&*snap.insurgency_runtime, &expected_insurgency);
    });

    // Reset persisted scalars back to Default and confirm deletions survive another reopen.
    reopened
        .tx("reset_substruct_scalars", |wtx| -> Result<(), TxError> {
            wtx.snap.war_state.next_negotiation_id = 0;
            wtx.snap.sov_runtime.next_mercenary_activity_seq = 0;
            wtx.snap.combat_crimewatch_state.extended_runtime_json.clear();
            Ok(())
        })
        .expect("reset scalars");
    drop(reopened);

    let reopened_after_reset = WorldRepository::open_sqlite(&path).expect("reopen after scalar reset");
    reopened_after_reset.with_snapshot(|snap| {
        assert_eq!(snap.war_state.next_negotiation_id, 0);
        assert_eq!(snap.sov_runtime.next_mercenary_activity_seq, 0);
        assert!(snap.combat_crimewatch_state.extended_runtime_json.is_empty());
        assert_eq!(snap.war_state.ally_expires_unix, expected_war.ally_expires_unix);
    });

    drop(reopened_after_reset);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn corp_crimewatch_legacy_extended_runtime_json_hydrates_sys_security_and_ess_ledger() {
    use crate::persistence::WorldRepository;
    use crate::sqlite_store::quarantine::LoadContext;

    let legacy_ext_json = serde_json::json!({
        "modified_system_securities": { "30000142": 0.45, "30002187": -0.25 },
        "ess_reserve_unlocked_until": { "30000142": 1_700_009_000u64 },
        "ess_reserve_bank_thefts": { "30000142": [[90_000_001i64, 250_000.0f64, 1_700_008_000u64]] },
        "ess_main_bank_thefts": { "30000142": [[90_000_002i64, 500_000.0f64, 1_700_008_500u64]] },
        "bounty_flush": 7
    })
    .to_string();

    // 1. SQLite `domain_rows` one-way load fallback (existing row in `extended_runtime_json` + non-promoted key).
    let mut conn = Connection::open_in_memory().unwrap();
    apply_migrations(&conn).unwrap();
    let encoded_scalar = serde_json::to_string(&legacy_ext_json).unwrap();
    conn.execute(
        "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'combat_crimewatch_state.extended_runtime_json', ?2)",
        params![DOMAIN_CRIMEWATCH, encoded_scalar],
    )
    .unwrap();

    let mut snap = WorldStateSnapshot::default();
    assert!(
        load_corp_crimewatch_domain_rows(&conn, &mut snap, &mut LoadContext::new(false)).unwrap(),
        "loading legacy promoted extended_runtime_json keys should report had_legacy=true"
    );
    assert_eq!(
        snap.combat_crimewatch_state.modified_system_securities.get(&30_000_142),
        Some(&0.45)
    );
    assert_eq!(
        snap.combat_crimewatch_state.modified_system_securities.get(&30_002_187),
        Some(&-0.25)
    );
    assert_eq!(
        snap.combat_crimewatch_state.ess_reserve_unlocked_until.get(&30_000_142),
        Some(&1_700_009_000)
    );
    assert_eq!(
        snap.combat_crimewatch_state.ess_reserve_bank_thefts.get(&30_000_142),
        Some(&vec![(90_000_001, 250_000.0, 1_700_008_000)])
    );
    assert_eq!(
        snap.combat_crimewatch_state.ess_main_bank_thefts.get(&30_000_142),
        Some(&vec![(90_000_002, 500_000.0, 1_700_008_500)])
    );
    let rem_ext: serde_json::Value =
        serde_json::from_str(&snap.combat_crimewatch_state.extended_runtime_json).unwrap();
    assert_eq!(rem_ext.get("bounty_flush"), Some(&serde_json::json!(7)));
    assert!(rem_ext.get("modified_system_securities").is_none());
    assert!(rem_ext.get("ess_reserve_unlocked_until").is_none());
    assert!(rem_ext.get("ess_reserve_bank_thefts").is_none());
    assert!(rem_ext.get("ess_main_bank_thefts").is_none());

    // Canonical sync writes typed domain_rows and removes promoted keys from extended_runtime_json.
    {
        let tx = conn.transaction().unwrap();
        assert!(sync_corp_crimewatch_domain_rows(&tx, &snap).unwrap());
        tx.commit().unwrap();
    }
    let stored_sec_val: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'combat_crimewatch_state.modified_system_securities/30000142'",
            params![DOMAIN_CRIMEWATCH],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored_sec_val, "0.45");
    let stored_ext_raw: String = conn
        .query_row(
            "SELECT value FROM domain_rows WHERE domain = ?1 AND key = 'combat_crimewatch_state.extended_runtime_json'",
            params![DOMAIN_CRIMEWATCH],
            |r| r.get(0),
        )
        .unwrap();
    let stored_ext_str: String = serde_json::from_str(&stored_ext_raw).unwrap();
    assert!(!stored_ext_str.contains("modified_system_securities"));
    assert!(!stored_ext_str.contains("ess_reserve_unlocked_until"));
    assert!(!stored_ext_str.contains("ess_reserve_bank_thefts"));
    assert!(!stored_ext_str.contains("ess_main_bank_thefts"));

    let mut clean_reload = WorldStateSnapshot::default();
    assert!(
        !load_corp_crimewatch_domain_rows(&conn, &mut clean_reload, &mut LoadContext::new(false))
            .unwrap()
    );
    assert_eq!(clean_reload.combat_crimewatch_state, snap.combat_crimewatch_state);

    // 2. Typed domain_rows value takes precedence over stale extended_runtime_json.
    let mut prec_state = PersistedCombatCrimewatchState::default();
    prec_state.modified_system_securities.insert(30_000_142, 0.9);
    prec_state.extended_runtime_json = legacy_ext_json.clone();
    assert!(hydrate_legacy_combat_extended_fields(&mut prec_state));
    assert_eq!(prec_state.modified_system_securities.get(&30_000_142), Some(&0.9));
    assert!(prec_state.ess_reserve_unlocked_until.contains_key(&30_000_142));
    assert!(!prec_state.extended_runtime_json.contains("modified_system_securities"));

    // 3. Old JSON snapshot fallback (`migrate_legacy_service_state`) + empty extended_runtime_json clearing.
    let only_promoted_json = serde_json::json!({
        "modified_system_securities": { "30000142": 0.15 },
        "ess_reserve_unlocked_until": { "30000142": 1_700_010_000u64 },
        "ess_reserve_bank_thefts": { "30000142": [[90_000_003i64, 125_000.0f64, 1_700_009_500u64]] },
        "ess_main_bank_thefts": { "30000142": [[90_000_004i64, 300_000.0f64, 1_700_009_600u64]] }
    })
    .to_string();
    let mut json_snap = WorldStateSnapshot::default();
    json_snap.combat_crimewatch_state.extended_runtime_json = only_promoted_json.clone();
    json_snap.migrate_legacy_service_state();
    assert_eq!(
        json_snap.combat_crimewatch_state.modified_system_securities.get(&30_000_142),
        Some(&0.15)
    );
    assert_eq!(
        json_snap.combat_crimewatch_state.ess_reserve_unlocked_until.get(&30_000_142),
        Some(&1_700_010_000)
    );
    assert_eq!(
        json_snap.combat_crimewatch_state.ess_reserve_bank_thefts.get(&30_000_142),
        Some(&vec![(90_000_003, 125_000.0, 1_700_009_500)])
    );
    assert_eq!(
        json_snap.combat_crimewatch_state.ess_main_bank_thefts.get(&30_000_142),
        Some(&vec![(90_000_004, 300_000.0, 1_700_009_600)])
    );
    assert!(json_snap.combat_crimewatch_state.extended_runtime_json.is_empty());

    // 4. End-to-end WorldRepository::open_sqlite migration flushes promoted domain_rows & cleans extended_runtime_json.
    let db_path = temp_corp_crimewatch_db_path("legacy_ext_runtime_migration_reopen");
    let _ = std::fs::remove_file(&db_path);
    {
        let raw_conn = Connection::open(&db_path).unwrap();
        apply_migrations(&raw_conn).unwrap();
        let raw_scalar = serde_json::to_string(&only_promoted_json).unwrap();
        raw_conn
            .execute(
                "INSERT INTO domain_rows (domain, key, value) VALUES (?1, 'combat_crimewatch_state.extended_runtime_json', ?2)",
                params![DOMAIN_CRIMEWATCH, raw_scalar],
            )
            .unwrap();
    }
    {
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite migrate");
        repo.with_snapshot(|s| {
            assert_eq!(s.combat_crimewatch_state.modified_system_securities.get(&30_000_142), Some(&0.15));
            assert_eq!(s.combat_crimewatch_state.ess_reserve_unlocked_until.get(&30_000_142), Some(&1_700_010_000));
            assert!(s.combat_crimewatch_state.extended_runtime_json.is_empty());
        });
    }
    let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen clean");
    reopened.with_snapshot(|s| {
        assert_eq!(s.combat_crimewatch_state.modified_system_securities.get(&30_000_142), Some(&0.15));
        assert_eq!(s.combat_crimewatch_state.ess_reserve_unlocked_until.get(&30_000_142), Some(&1_700_010_000));
        assert!(s.combat_crimewatch_state.extended_runtime_json.is_empty());
    });
    drop(reopened);
    let _ = std::fs::remove_file(&db_path);
}
