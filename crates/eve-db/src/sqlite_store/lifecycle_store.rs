//! Lifecycle runtime `domain_rows` persistence with exhaustive struct destructuring (`#1490`).

use super::domain_rows::{
    decode_map_entry, diff_cow_map_field, diff_scalar, fetch_domain_row_entries,
    from_checked_domain_json, table_exists, DomainRowChange, DomainSyncState, LegacyMerge,
    Persist, DOMAIN_SP6_LIFECYCLE,
};
use rusqlite::{Connection, Transaction};

#[cfg(test)]
pub(crate) const LEGACY_SP6_LIFECYCLE_STATE_SQL: &str = "
CREATE TABLE IF NOT EXISTS sp6_lifecycle_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    state_json TEXT NOT NULL
);";

macro_rules! for_each_lifecycle_field {
    ($cb:ident) => {
        $cb!(
            maps: [
                safe_logoffs,
                disconnected_ships,
                self_destructs,
                abyssal_sessions,
                pvp_timers_until,
                npc_timers_until,
                weapon_timers_until,
                criminal_timers_until,
                ship_combat_hp,
                space_object_meta,
                active_scanner_probes,
                item_velocities,
                session_change_timers,
            ],
            scalars: [
                last_downtime_day_idx,
                server_runtime_json,
            ]
        );
    };
}

macro_rules! impl_lifecycle_persist {
    (maps: [$($map_f:ident),* $(,)?], scalars: [$($scalar_f:ident),* $(,)?]) => {
        impl Persist for crate::Sp6PersistedState {
            fn collect_diff_ops(
                &self,
                other: &Self,
                domain: &'static str,
                out: &mut Vec<DomainRowChange>,
            ) {
                let Self {
                    $($map_f: _,)*
                    $($scalar_f: _,)*
                    // Session-ephemeral: paired with non-persisted actor `active_nanite_repairs` and per-actor post-unlock seq.
                    nanite_paste_shortfall: _,
                    post_unlock_committed_seq: _,
                } = self;
                let def = Self::default();
                $(
                    diff_cow_map_field(
                        domain,
                        "",
                        stringify!($map_f),
                        &self.$map_f,
                        &other.$map_f,
                        out,
                    );
                )*
                $(
                    diff_scalar(
                        domain,
                        stringify!($scalar_f),
                        &self.$scalar_f,
                        &other.$scalar_f,
                        &def.$scalar_f,
                        out,
                    );
                )*
            }

            fn sync_full(&self, tx: &Transaction<'_>, domain: &'static str) -> Result<bool, String> {
                let mut state = DomainSyncState::load(tx, domain)?;
                let def = Self::default();
                $(
                    state.push_map(stringify!($map_f), self.$map_f.iter())?;
                )*
                $(
                    state.push_scalar(stringify!($scalar_f), &self.$scalar_f, &def.$scalar_f)?;
                )*
                state.finish(tx)
            }

            fn load_domain(
                conn: &Connection,
                domain: &'static str,
                ctx: &mut super::quarantine::LoadContext,
            ) -> Result<Self, String> {
                let rows = fetch_domain_row_entries(conn, domain)?;
                let mut out = Self::default();
                for (rowid, raw_key, raw_val) in rows {
                    let split = match raw_key.split_once('/') {
                        Some((f, sub)) => (f, Some(sub)),
                        None => (raw_key.as_str(), None),
                    };
                    let res: Result<(), String> = match split {
                        $(
                            (stringify!($map_f), Some(sub)) => {
                                decode_map_entry(sub, &raw_val).map(|(k, v)| {
                                    out.$map_f.insert(k, v);
                                })
                            }
                        )*
                        $(
                            (stringify!($scalar_f), None) => {
                                from_checked_domain_json(&raw_val).map(|v| {
                                    out.$scalar_f = v;
                                })
                            }
                        )*
                        // Retired Track 3 (#1496) sp6_lifecycle mission keys: consumed during
                        // mission_instances migration in `load.rs` and ignored here.
                        ("missions_awaiting_downtime_respawn", None)
                        | ("mission_initial_npc_counts", Some(_))
                        | ("mission_initial_npc_types", Some(_)) => Ok(()),
                        _ => Err(format!("unknown domain_rows key '{raw_key}' for {domain}")),
                    };
                    if let Err(err) = res {
                        ctx.record_bad_row(
                            "domain_rows",
                            domain,
                            raw_key,
                            raw_val,
                            err,
                            Some(rowid),
                        )?;
                    }
                }
                Ok(out)
            }
        }

        impl LegacyMerge for crate::Sp6PersistedState {
            fn merge_legacy(&mut self, legacy: Self) {
                let def = Self::default();
                $(
                    for (k, v) in legacy.$map_f {
                        self.$map_f.entry(k).or_insert(v);
                    }
                )*
                $(
                    if self.$scalar_f == def.$scalar_f && legacy.$scalar_f != def.$scalar_f {
                        self.$scalar_f = legacy.$scalar_f;
                    }
                )*
            }
        }
    };
}

for_each_lifecycle_field!(impl_lifecycle_persist);

/// Loads lifecycle runtime state from `domain_rows` and migrates any remaining legacy
/// `sp6_lifecycle_state` or `system_checkpoint(kind = 'session_change_timers')` rows if present
/// (`domain_rows` entries win on collision).
///
/// Fails closed if any `domain_rows` or legacy row fails checked parsing and quarantine is disabled,
/// without mutating `snap.sp6_lifecycle`. Returns `true` if any legacy lifecycle source existed.
pub(crate) fn load_sp6_lifecycle_domain_rows(
    conn: &Connection,
    snap: &mut crate::WorldStateSnapshot,
    ctx: &mut super::quarantine::LoadContext,
) -> Result<bool, String> {
    let mut staged = crate::Sp6PersistedState::load_domain(conn, DOMAIN_SP6_LIFECYCLE, ctx)?;
    let had_legacy_table = table_exists(conn, "sp6_lifecycle_state")?;

    if had_legacy_table {
        let mut stmt = conn
            .prepare("SELECT rowid, singleton_id, state_json FROM sp6_lifecycle_state")
            .map_err(|e| format!("legacy sp6_lifecycle_state prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy sp6_lifecycle_state query: {e}"))?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            let rowid = r.get::<_, i64>(0).ok();
            let key = super::quarantine::row_col_string(r, 1);
            let raw_payload = super::quarantine::row_col_string(r, 2);
            let parse_res = r
                .get::<_, String>(2)
                .map_err(|e| e.to_string())
                .and_then(|s| from_checked_domain_json::<crate::Sp6PersistedState>(&s));
            match parse_res {
                Ok(legacy) => staged.merge_legacy(legacy),
                Err(err) => {
                    ctx.record_bad_row(
                        "sp6_lifecycle_state",
                        "sp6_lifecycle_state",
                        key,
                        raw_payload,
                        err,
                        rowid,
                    )?;
                }
            }
        }
    }

    let mut had_legacy_checkpoint_timers = false;
    if table_exists(conn, "system_checkpoint")? {
        let mut stmt = conn
            .prepare(
                "SELECT key, value FROM system_checkpoint
                 WHERE kind = 'session_change_timers'
                 ORDER BY system_id, key",
            )
            .map_err(|e| format!("legacy system_checkpoint session_change_timers prepare: {e}"))?;
        let mut rows = stmt
            .query([])
            .map_err(|e| format!("legacy system_checkpoint session_change_timers query: {e}"))?;
        while let Some(r) = rows.next().map_err(|e| e.to_string())? {
            had_legacy_checkpoint_timers = true;
            let key = super::quarantine::row_col_string(r, 0);
            let raw_payload = super::quarantine::row_col_string(r, 1);
            let parse_res = (|| -> Result<(i64, crate::SessionChangeTimerRecord), String> {
                let raw_key = r.get::<_, String>(0).map_err(|e| e.to_string())?;
                let char_id = raw_key
                    .parse::<i64>()
                    .map_err(|e| format!("invalid session_change_timers key '{raw_key}': {e}"))?;
                let raw_json = r.get::<_, String>(1).map_err(|e| e.to_string())?;
                let rec = from_checked_domain_json::<crate::SessionChangeTimerRecord>(&raw_json)?;
                Ok((char_id, rec))
            })();
            match parse_res {
                Ok((char_id, rec)) => {
                    staged.session_change_timers.entry(char_id).or_insert(rec);
                }
                Err(err) => {
                    ctx.record_bad_row(
                        "system_checkpoint",
                        "system_checkpoint",
                        key,
                        raw_payload,
                        err,
                        None,
                    )?;
                }
            }
        }
    }

    snap.sp6_lifecycle = staged.into();
    Ok(had_legacy_table || had_legacy_checkpoint_timers)
}

/// Drops the retired legacy `sp6_lifecycle_state` table and deletes migrated
/// `kind = 'session_change_timers'` rows from `system_checkpoint` once flushed into `domain_rows`.
pub(crate) fn drop_legacy_sp6_lifecycle_table(tx: &Transaction<'_>) -> Result<(), String> {
    tx.execute_batch("DROP TABLE IF EXISTS sp6_lifecycle_state;")
        .map_err(|e| format!("drop legacy sp6_lifecycle_state table: {e}"))?;
    if table_exists(tx, "system_checkpoint")? {
        tx.execute(
            "DELETE FROM system_checkpoint WHERE kind = 'session_change_timers'",
            [],
        )
        .map_err(|e| format!("delete migrated system_checkpoint session_change_timers: {e}"))?;
    }
    Ok(())
}

/// Synchronizes lifecycle runtime state into `domain_rows`.
pub(crate) fn sync_sp6_lifecycle_domain_rows(
    tx: &Transaction<'_>,
    snap: &crate::WorldStateSnapshot,
) -> Result<bool, String> {
    let inner: &crate::Sp6PersistedState = &snap.sp6_lifecycle;
    inner.sync_full(tx, DOMAIN_SP6_LIFECYCLE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::WorldRepository;
    use crate::sqlite_store::domain_rows::{apply_domain_row_changes, validate_domain_row_changes};
    use crate::tx_engine::TxError;

    struct TempDirGuard(std::path::PathBuf);
    impl TempDirGuard {
        fn new(label: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "evomu_lifecycle_domain_rows_{label}_{}_{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create tempdir");
            Self(dir)
        }
    }
    impl Drop for TempDirGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn lifecycle_domain_rows_legacy_migration_roundtrip_and_table_drop() {
        let dir = TempDirGuard::new("legacy_lifecycle");
        let db_path = dir.0.join("world.sqlite");

        let mut legacy = crate::Sp6PersistedState::default();
        legacy.safe_logoffs.insert(
            90_000_001,
            crate::SafeLogoffEntry {
                char_id: 90_000_001,
                ship_id: 100_000_001,
                solar_system_id: 30_000_142,
                started_unix: 1_700_000_000,
                complete_at_unix: 1_700_000_030,
                completed: false,
            },
        );
        legacy.disconnected_ships.insert(
            90_000_002,
            crate::DisconnectedShipEntry {
                char_id: 90_000_002,
                ship_id: 100_000_002,
                ship_type_id: 587,
                solar_system_id: 30_000_142,
                disconnected_at_unix: 1_700_000_100,
                remove_at_unix: 1_700_000_900,
                emergency_warp_dest: Some([10.0, 20.0, 30.0]),
                last_position: [1.0, 2.0, 3.0],
                last_velocity: [4.0, -5.0, 6.0],
                removed_from_space: false,
                pre_warp_position: Some([0.0, 0.0, 0.0]),
            },
        );
        legacy.self_destructs.insert(
            100_000_003,
            crate::SelfDestructPersistedEntry {
                ship_id: 100_000_003,
                char_id: 90_000_003,
                solar_system_id: 30_000_142,
                started_unix: 1_700_000_200,
                detonate_at_unix: 1_700_000_320,
            },
        );
        legacy.abyssal_sessions.insert(
            90_000_004,
            crate::AbyssalPersistedSession {
                char_id: 90_000_004,
                ship_id: 100_000_004,
                pocket_system_id: 32_000_001,
                origin_system_id: 30_000_142,
                entered_unix: 1_700_000_010,
                collapse_at_unix: 1_700_001_210,
            },
        );
        legacy.pvp_timers_until.insert(90_000_001, 5_000);
        legacy.npc_timers_until.insert(90_000_001, 6_000);
        legacy.weapon_timers_until.insert(90_000_001, 7_000);
        legacy.criminal_timers_until.insert(90_000_001, 8_000);
        legacy.last_downtime_day_idx = Some(19_850);
        legacy.ship_combat_hp.insert(
            100_000_001,
            crate::PersistedCombatHp {
                shield_hp: 400.5,
                armor_hp: 300.25,
                hull_hp: 250.0,
            },
        );
        legacy.space_object_meta.insert(
            800_000_001,
            crate::PersistedSpaceObjectMeta {
                created_unix: Some(1_700_000_050),
                is_npc_wreck: true,
                anchor_start_unix: Some(1_700_000_060),
                anchoring_delay_secs: 45.0,
                anchor_completed: true,
                bubble_radius_m: Some(20_000.0),
                drone_ball_id: Some(800_000_002),
                planet_id: Some(40_009_082),
            },
        );
        legacy.active_scanner_probes.insert(
            900_000_001,
            crate::PersistedScannerProbe {
                probe_id: 900_000_001,
                probe_type_id: 30_013,
                owner_char_id: 90_000_001,
                ship_id: 100_000_001,
                launcher_module_id: 100_000_002,
                solar_system_id: 30_000_142,
                position: [10.0, 20.0, 30.0],
                destination: [40.0, 50.0, 60.0],
                range_step: 4,
                scan_range_m: 1_000_000.0,
                base_sensor_strength: 40.0,
                max_flight_time_s: 3_600.0,
                launched_at_unix: 1_700_000_000,
                state: 1,
                warp_end_ft: Some(133_000_000_000),
            },
        );
        legacy
            .item_velocities
            .insert(100_000_001, [120.0, -45.0, 30.0]);
        legacy.server_runtime_json = Some("{\"active_warps\":{}}".to_string());

        {
            let conn = Connection::open(&db_path).expect("open sqlite");
            crate::sqlite_store::apply_migrations(&conn).expect("migrations");
            conn.execute_batch(LEGACY_SP6_LIFECYCLE_STATE_SQL)
                .expect("create legacy table");
            let blob = serde_json::to_string(&legacy).expect("serialize legacy");
            conn.execute(
                "INSERT INTO sp6_lifecycle_state (singleton_id, state_json) VALUES (1, ?1)",
                rusqlite::params![blob],
            )
            .expect("insert legacy blob");
            // Seed an existing `domain_rows` key to verify `domain_rows` wins on collision.
            conn.execute(
                "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![DOMAIN_SP6_LIFECYCLE, "pvp_timers_until/90000001", "99999"],
            )
            .expect("insert domain_rows override");
        }

        let mut expected = legacy.clone();
        expected.pvp_timers_until.insert(90_000_001, 99_999);

        let repo = WorldRepository::open_sqlite(&db_path).expect("open migrated repo");
        repo.with_snapshot(|snap| {
            let actual: &crate::Sp6PersistedState = &snap.sp6_lifecycle;
            assert_eq!(actual, &expected);
        });
        repo.with_sqlite_conn(|conn| {
            assert!(
                !table_exists(conn, "sp6_lifecycle_state").expect("check legacy dropped"),
                "sp6_lifecycle_state must be dropped after migration"
            );
            let row_count: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM domain_rows WHERE domain = ?1",
                    rusqlite::params![DOMAIN_SP6_LIFECYCLE],
                    |r| r.get(0),
                )
                .expect("count lifecycle domain_rows");
            assert_eq!(row_count, 14);
        });
        drop(repo);

        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen after migration");
        reopened.with_snapshot(|snap| {
            let actual: &crate::Sp6PersistedState = &snap.sp6_lifecycle;
            assert_eq!(actual, &expected);
        });
    }

    #[test]
    fn lifecycle_domain_rows_single_entry_diff_zero_cold_clones_and_non_finite_rejection() {
        let dir = TempDirGuard::new("lifecycle_diff");
        let db_path = dir.0.join("world.sqlite");
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

        let seed_res: Result<(), TxError> = repo.tx("seed_lifecycle", |wtx| {
            for i in 1..=20i64 {
                wtx.snap.sp6_lifecycle.pvp_timers_until.insert(i, (i as u64) * 100);
            }
            wtx.snap.sp6_lifecycle.ship_combat_hp.insert(
                100_000_001,
                crate::PersistedCombatHp {
                    shield_hp: 400.0,
                    armor_hp: 300.0,
                    hull_hp: 200.0,
                },
            );
            wtx.snap
                .sp6_lifecycle
                .item_velocities
                .insert(100_000_002, [10.0, 20.0, 30.0]);
            wtx.snap.sp6_lifecycle.last_downtime_day_idx = Some(19_850);
            Ok(())
        });
        seed_res.expect("seed lifecycle");

        crate::persistence::reset_thread_cold_domain_syncs_for_test();
        let update_res: Result<(), TxError> = repo.tx("update_one_timer", |wtx| {
            wtx.snap.sp6_lifecycle.pvp_timers_until.insert(5, 888_888);
            Ok(())
        });
        update_res.expect("update single timer");
        assert_eq!(
            crate::persistence::thread_cold_domain_syncs_for_test(),
            0,
            "lifecycle mutation must trigger 0 cold domain syncs"
        );

        let before_snap = repo.with_snapshot(Clone::clone);
        let mut after_snap = before_snap.clone();
        after_snap.sp6_lifecycle.pvp_timers_until.insert(6, 777_777);
        let changes = crate::tx_engine::ChangeSet::diff(2, &before_snap, &after_snap);
        assert_eq!(changes.domain_rows.len(), 1);
        assert!(!changes.cold_domains.any());

        let mut mem_conn = Connection::open_in_memory().expect("open mem sqlite");
        crate::sqlite_store::apply_migrations(&mem_conn).expect("migrate mem");
        crate::sqlite_store::replace_sqlite_from_snapshot(&mut mem_conn, &before_snap)
            .expect("seed mem");
        let before_changes = mem_conn.total_changes();
        {
            let sql_tx = mem_conn.transaction().expect("begin tx");
            let mut exec =
                crate::tx_engine::TxSqlExecutor::new(&sql_tx, crate::tx_engine::TxFaultPlan::None);
            apply_domain_row_changes(&mut exec, &changes.domain_rows).expect("apply 1 row");
            sql_tx.commit().expect("commit 1 row");
        }
        assert_eq!(
            mem_conn.total_changes() - before_changes,
            1,
            "mutating one lifecycle map entry must write strictly 1 SQLite row"
        );

        // Clearing a scalar resets its row via DELETE and reloads cleanly as None.
        repo.tx("clear_downtime_idx", |wtx| -> Result<(), TxError> {
            wtx.snap.sp6_lifecycle.last_downtime_day_idx = None;
            Ok(())
        })
        .expect("clear downtime day idx");
        drop(repo);
        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen cleared scalar");
        reopened.with_snapshot(|snap| {
            assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, None);
            assert_eq!(snap.sp6_lifecycle.pvp_timers_until.get(&5), Some(&888_888));
        });

        // Non-finite float rejection inside both `ship_combat_hp` and `item_velocities`
        // must name the domain and exact row key, without mutating committed snapshot state.
        let clean_snap = reopened.with_snapshot(Clone::clone);
        for bad_val in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut nan_hp = clean_snap.clone();
            nan_hp
                .sp6_lifecycle
                .ship_combat_hp
                .get_mut(&100_000_001)
                .expect("seeded hp")
                .shield_hp = bad_val;
            let hp_changes = crate::tx_engine::ChangeSet::diff(3, &clean_snap, &nan_hp);
            let hp_err = validate_domain_row_changes(&hp_changes.domain_rows, 1024 * 1024)
                .expect_err("non-finite combat hp must fail validation");
            assert!(matches!(hp_err, TxError::NonFinite { .. }), "{hp_err}");
            let hp_msg = hp_err.to_string();
            assert!(hp_msg.contains(DOMAIN_SP6_LIFECYCLE), "{hp_msg}");
            assert!(hp_msg.contains("ship_combat_hp/100000001"), "{hp_msg}");

            let tx_err: Result<(), TxError> = reopened.tx("bad_velocity", |wtx| {
                wtx.snap
                    .sp6_lifecycle
                    .item_velocities
                    .insert(100_000_002, [bad_val, 0.0, 0.0]);
                Ok(())
            });
            let vel_msg = tx_err
                .expect_err("non-finite item velocity must fail repo.tx")
                .to_string();
            assert!(vel_msg.contains(DOMAIN_SP6_LIFECYCLE), "{vel_msg}");
            assert!(vel_msg.contains("item_velocities/100000002"), "{vel_msg}");
        }
        reopened.with_snapshot(|snap| {
            assert_eq!(snap.sp6_lifecycle.ship_combat_hp[&100_000_001].shield_hp, 400.0);
            assert_eq!(snap.sp6_lifecycle.item_velocities[&100_000_002], [10.0, 20.0, 30.0]);
        });
    }

    #[test]
    fn lifecycle_domain_rows_load_fails_closed_on_corrupt_domain_or_legacy_row() {
        let conn = Connection::open_in_memory().expect("open");
        crate::sqlite_store::apply_migrations(&conn).expect("migrations");

        conn.execute(
            "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3), (?1, ?4, ?5)",
            rusqlite::params![
                DOMAIN_SP6_LIFECYCLE,
                "pvp_timers_until/10",
                "1234",
                "ship_combat_hp/20",
                "{corrupt-json"
            ],
        )
        .expect("insert domain rows");

        let mut snap = crate::WorldStateSnapshot::default();
        snap.sp6_lifecycle.pvp_timers_until.insert(99, 999);
        snap.sp6_lifecycle.last_downtime_day_idx = Some(777);

        let mut ctx = crate::sqlite_store::LoadContext::new(false);
        let err = load_sp6_lifecycle_domain_rows(&conn, &mut snap, &mut ctx).unwrap_err();
        assert!(err.contains(DOMAIN_SP6_LIFECYCLE), "unexpected err: {err}");
        assert_eq!(snap.sp6_lifecycle.pvp_timers_until.get(&99), Some(&999));
        assert!(!snap.sp6_lifecycle.pvp_timers_until.contains_key(&10));
        assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(777));

        conn.execute("DELETE FROM domain_rows", []).expect("clear");
        conn.execute_batch(LEGACY_SP6_LIFECYCLE_STATE_SQL)
            .expect("create legacy table");
        conn.execute(
            "INSERT INTO sp6_lifecycle_state (singleton_id, state_json) VALUES (1, '{corrupt')",
            [],
        )
        .expect("insert corrupt legacy");
        let legacy_err = load_sp6_lifecycle_domain_rows(&conn, &mut snap, &mut ctx).unwrap_err();
        assert!(
            legacy_err.contains("sp6_lifecycle_state"),
            "unexpected legacy err: {legacy_err}"
        );
        assert_eq!(snap.sp6_lifecycle.pvp_timers_until.get(&99), Some(&999));
        assert_eq!(snap.sp6_lifecycle.last_downtime_day_idx, Some(777));
    }

    #[test]
    fn lifecycle_domain_rows_tx_commit_roundtrip_persists_all_fields_across_restart() {
        let dir = TempDirGuard::new("tx_roundtrip_all_fields");
        let db_path = dir.0.join("world.sqlite");
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

        let expected = crate::Sp6PersistedState {
            safe_logoffs: [(
                90_000_011,
                crate::SafeLogoffEntry {
                    char_id: 90_000_011,
                    ship_id: 100_000_011,
                    solar_system_id: 30_000_142,
                    started_unix: 1_700_000_001,
                    complete_at_unix: 1_700_000_031,
                    completed: true,
                },
            )]
            .into_iter()
            .collect(),
            disconnected_ships: [(
                90_000_012,
                crate::DisconnectedShipEntry {
                    char_id: 90_000_012,
                    ship_id: 100_000_012,
                    ship_type_id: 587,
                    solar_system_id: 30_000_142,
                    disconnected_at_unix: 1_700_000_100,
                    remove_at_unix: 1_700_000_900,
                    emergency_warp_dest: Some([11.0, 22.0, 33.0]),
                    last_position: [1.5, 2.5, 3.5],
                    last_velocity: [4.5, -5.5, 6.5],
                    removed_from_space: true,
                    pre_warp_position: Some([-1.0, -2.0, -3.0]),
                },
            )]
            .into_iter()
            .collect(),
            self_destructs: [(
                100_000_013,
                crate::SelfDestructPersistedEntry {
                    ship_id: 100_000_013,
                    char_id: 90_000_013,
                    solar_system_id: 30_000_142,
                    started_unix: 1_700_000_200,
                    detonate_at_unix: 1_700_000_320,
                },
            )]
            .into_iter()
            .collect(),
            abyssal_sessions: [(
                90_000_014,
                crate::AbyssalPersistedSession {
                    char_id: 90_000_014,
                    ship_id: 100_000_014,
                    pocket_system_id: 32_000_002,
                    origin_system_id: 30_000_142,
                    entered_unix: 1_700_000_010,
                    collapse_at_unix: 1_700_001_210,
                },
            )]
            .into_iter()
            .collect(),
            pvp_timers_until: [(90_000_011, 10_001)].into_iter().collect(),
            npc_timers_until: [(90_000_011, 10_002)].into_iter().collect(),
            weapon_timers_until: [(90_000_011, 10_003)].into_iter().collect(),
            criminal_timers_until: [(90_000_011, 10_004)].into_iter().collect(),
            last_downtime_day_idx: Some(19_851),
            ship_combat_hp: [(
                100_000_011,
                crate::PersistedCombatHp {
                    shield_hp: 410.5,
                    armor_hp: 310.25,
                    hull_hp: 210.0,
                },
            )]
            .into_iter()
            .collect(),
            space_object_meta: [(
                800_000_011,
                crate::PersistedSpaceObjectMeta {
                    created_unix: Some(1_700_000_055),
                    is_npc_wreck: false,
                    anchor_start_unix: Some(1_700_000_065),
                    anchoring_delay_secs: 60.0,
                    anchor_completed: true,
                    bubble_radius_m: Some(25_000.0),
                    drone_ball_id: Some(800_000_012),
                    planet_id: Some(40_009_083),
                },
            )]
            .into_iter()
            .collect(),
            active_scanner_probes: [(
                900_000_011,
                crate::PersistedScannerProbe {
                    probe_id: 900_000_011,
                    probe_type_id: 30_013,
                    owner_char_id: 90_000_011,
                    ship_id: 100_000_011,
                    launcher_module_id: 100_000_015,
                    solar_system_id: 30_000_142,
                    position: [15.0, 25.0, 35.0],
                    destination: [45.0, 55.0, 65.0],
                    range_step: 5,
                    scan_range_m: 2_000_000.0,
                    base_sensor_strength: 44.0,
                    max_flight_time_s: 3_600.0,
                    launched_at_unix: 1_700_000_011,
                    state: 2,
                    warp_end_ft: Some(133_000_000_111),
                },
            )]
            .into_iter()
            .collect(),
            item_velocities: [(100_000_011, [110.0, -20.0, 30.0])]
                .into_iter()
                .collect(),
            session_change_timers: [(
                90_000_011,
                crate::SessionChangeTimerRecord {
                    expires_unix: 1_700_000_010,
                    system_id: 30_000_142,
                    started_tick: 42,
                    reason: "stargate_jump".to_string(),
                },
            )]
            .into_iter()
            .collect(),
            server_runtime_json: Some("{\"roundtrip\":true}".to_string()),
            nanite_paste_shortfall: Default::default(),
            post_unlock_committed_seq: Default::default(),
        };

        repo.tx("seed_all_lifecycle_fields", |wtx| -> Result<(), TxError> {
            wtx.snap.sp6_lifecycle = expected.clone().into();
            Ok(())
        })
        .expect("commit all lifecycle fields");
        drop(repo);

        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen sqlite");
        reopened.with_snapshot(|snap| {
            let actual: &crate::Sp6PersistedState = &snap.sp6_lifecycle;
            assert_eq!(actual, &expected);
        });
    }

    #[test]
    fn session_change_timers_domain_rows_diff_roundtrip_and_legacy_checkpoint_migration() {
        let dir = TempDirGuard::new("session_change_timers_migration");
        let db_path = dir.0.join("world.sqlite");

        // 1. Incremental ChangeSet::diff + DOMAIN_SP6_LIFECYCLE round-trip
        {
            let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
            let before_snap = repo.with_snapshot(Clone::clone);
            let mut after_snap = before_snap.clone();
            let rec = crate::SessionChangeTimerRecord {
                expires_unix: 1_800_000_010,
                system_id: 30_000_142,
                started_tick: 15,
                reason: "undock".to_string(),
            };
            after_snap
                .sp6_lifecycle
                .session_change_timers
                .insert(90_000_101, rec.clone());
            let changes = crate::tx_engine::ChangeSet::diff(1, &before_snap, &after_snap);
            assert_eq!(changes.domain_rows.len(), 1);
            assert!(matches!(
                &changes.domain_rows[0],
                DomainRowChange::Upsert { domain, key, .. }
                    if *domain == DOMAIN_SP6_LIFECYCLE && key == "session_change_timers/90000101"
            ));

            repo.tx("seed_timer", |wtx| -> Result<(), TxError> {
                wtx.snap
                    .sp6_lifecycle
                    .session_change_timers
                    .insert(90_000_101, rec.clone());
                Ok(())
            })
            .expect("commit timer");

            // Single-entry delete produces strictly 1 domain_rows delete op.
            let mid_snap = repo.with_snapshot(Clone::clone);
            let mut deleted_snap = mid_snap.clone();
            deleted_snap
                .sp6_lifecycle
                .session_change_timers
                .remove(&90_000_101);
            let del_changes = crate::tx_engine::ChangeSet::diff(2, &mid_snap, &deleted_snap);
            assert_eq!(del_changes.domain_rows.len(), 1);
            assert!(matches!(
                &del_changes.domain_rows[0],
                DomainRowChange::Delete { domain, key }
                    if *domain == DOMAIN_SP6_LIFECYCLE && key == "session_change_timers/90000101"
            ));
            drop(repo);

            let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen timer repo");
            reopened.with_snapshot(|snap| {
                assert_eq!(
                    snap.sp6_lifecycle.session_change_timers.get(&90_000_101),
                    Some(&rec)
                );
            });
        }

        // 2. One-way legacy load from `system_checkpoint(kind = 'session_change_timers')`
        //    including legacy `location_change_invul_until` JSON field, `domain_rows` winning
        //    on collision, and cleanup of migrated `system_checkpoint` rows.
        let legacy_db_path = dir.0.join("legacy_cp.sqlite");
        {
            let conn = Connection::open(&legacy_db_path).expect("open legacy sqlite");
            crate::sqlite_store::apply_migrations(&conn).expect("migrations");
            conn.execute(
                "INSERT INTO domain_rows (domain, key, value) VALUES (?1, ?2, ?3)",
                rusqlite::params![
                    DOMAIN_SP6_LIFECYCLE,
                    "session_change_timers/90000001",
                    serde_json::to_string(&crate::SessionChangeTimerRecord {
                        expires_unix: 1_900_000_999,
                        system_id: 30_000_144,
                        started_tick: 88,
                        reason: "domain_wins".to_string(),
                    })
                    .unwrap()
                ],
            )
            .expect("insert colliding domain_rows timer");
            conn.execute(
                "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
                 VALUES (?1, 'session_change_timers', ?2, ?3, ?4),
                        (?1, 'session_change_timers', ?5, ?6, ?4)",
                rusqlite::params![
                    0_i64,
                    "90000001",
                    r#"{"expires_unix":1800000001,"system_id":30000142,"started_tick":1,"reason":"stale_cp","location_change_invul_until":1800000060}"#,
                    1_800_000_000_i64,
                    "90000002",
                    r#"{"expires_unix":1800000020,"system_id":30002187,"started_tick":7,"reason":"stargate_jump","location_change_invul_until":1800000075}"#,
                ],
            )
            .expect("insert legacy system_checkpoint timers");
        }

        let migrated = WorldRepository::open_sqlite(&legacy_db_path).expect("open migrated");
        migrated.with_snapshot(|snap| {
            let winner = snap
                .sp6_lifecycle
                .session_change_timers
                .get(&90_000_001)
                .expect("colliding timer");
            assert_eq!(winner.expires_unix, 1_900_000_999);
            assert_eq!(winner.reason, "domain_wins");

            let migrated_rec = snap
                .sp6_lifecycle
                .session_change_timers
                .get(&90_000_002)
                .expect("migrated timer with legacy invul field");
            assert_eq!(migrated_rec.expires_unix, 1_800_000_020);
            assert_eq!(migrated_rec.system_id, 30_002_187);
            assert_eq!(migrated_rec.started_tick, 7);
            assert_eq!(migrated_rec.reason, "stargate_jump");
        });
        migrated.with_sqlite_conn(|conn| {
            let remaining_cp: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM system_checkpoint WHERE kind = 'session_change_timers'",
                    [],
                    |r| r.get(0),
                )
                .expect("count remaining legacy rows");
            assert_eq!(remaining_cp, 0);
        });
        drop(migrated);

        // 3. Corrupt legacy `system_checkpoint` row fails closed without mutating prior snapshot.
        let bad_conn = Connection::open_in_memory().expect("open mem");
        crate::sqlite_store::apply_migrations(&bad_conn).expect("migrations");
        bad_conn
            .execute(
                "INSERT INTO system_checkpoint (system_id, kind, key, value, updated_unix)
                 VALUES (0, 'session_change_timers', '90000003', '{corrupt-json', 100)",
                [],
            )
            .expect("insert corrupt cp row");
        let mut snap = crate::WorldStateSnapshot::default();
        snap.sp6_lifecycle.session_change_timers.insert(
            77,
            crate::SessionChangeTimerRecord {
                expires_unix: 555,
                system_id: 30_000_142,
                started_tick: 1,
                reason: "keep".to_string(),
            },
        );
        let mut load_ctx = crate::sqlite_store::LoadContext::new(false);
        let err = load_sp6_lifecycle_domain_rows(&bad_conn, &mut snap, &mut load_ctx).unwrap_err();
        assert!(err.contains("system_checkpoint"), "unexpected err: {err}");
        assert_eq!(
            snap.sp6_lifecycle.session_change_timers.get(&77).map(|r| r.expires_unix),
            Some(555)
        );
    }
}
