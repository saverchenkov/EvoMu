//! Fail-closed row validation and `--quarantine-unreadable` quarantine table support (`#906`, `#856`).

use rusqlite::{params, types::ValueRef, Connection, Row};
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

/// A corrupted or unreadable SQLite row moved into `quarantine_rows` when quarantine mode is active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantinedRow {
    pub source_table: String,
    pub key: String,
    pub payload: String,
    pub error: String,
    pub quarantined_unix: u64,
}

thread_local! {
    static TEST_QUARANTINE_OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
}

static GLOBAL_QUARANTINE_OVERRIDE: AtomicU8 = AtomicU8::new(0);

/// Sets a process-wide override for `--quarantine-unreadable` behavior.
pub fn set_quarantine_unreadable(enabled: bool) {
    GLOBAL_QUARANTINE_OVERRIDE.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
}

/// Clears any process-wide override set via [`set_quarantine_unreadable`].
pub fn clear_quarantine_unreadable_override() {
    GLOBAL_QUARANTINE_OVERRIDE.store(0, Ordering::Relaxed);
}

/// Runs `f` with a thread-local `--quarantine-unreadable` override (safe for parallel tests).
pub fn with_quarantine_unreadable_for_test<R>(enabled: bool, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<bool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            TEST_QUARANTINE_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let prev = TEST_QUARANTINE_OVERRIDE.with(|c| {
        let old = c.get();
        c.set(Some(enabled));
        old
    });
    let _guard = Guard(prev);
    f()
}

/// Parses whether `--quarantine-unreadable` (or `--quarantine-unreadable=true/1`) is present in CLI args.
pub fn parse_cli_quarantine_unreadable<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for raw in args {
        let a = raw.as_ref().trim();
        if a == "--" {
            break;
        }
        if a == "--quarantine-unreadable" {
            return true;
        }
        if let Some(rest) = a.strip_prefix("--quarantine-unreadable=") {
            let v = rest.trim().to_ascii_lowercase();
            if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
                return true;
            }
            if matches!(v.as_str(), "0" | "false" | "no" | "off") {
                return false;
            }
        }
    }
    false
}

/// Determines whether unreadable SQLite rows should be quarantined instead of failing startup closed.
pub fn default_quarantine_unreadable() -> bool {
    if let Some(v) = TEST_QUARANTINE_OVERRIDE.with(|c| c.get()) {
        return v;
    }
    match GLOBAL_QUARANTINE_OVERRIDE.load(Ordering::Relaxed) {
        1 => return false,
        2 => return true,
        _ => {}
    }
    if let Ok(env_val) = std::env::var("EVOMU_QUARANTINE_UNREADABLE") {
        let v = env_val.trim().to_ascii_lowercase();
        if matches!(v.as_str(), "1" | "true" | "yes" | "on") {
            return true;
        }
        if matches!(v.as_str(), "0" | "false" | "no" | "off") {
            return false;
        }
    }
    parse_cli_quarantine_unreadable(std::env::args().skip(1))
}

/// Tracks row-validation policy and quarantined rows during a SQLite snapshot load.
pub struct LoadContext {
    pub quarantine_unreadable: bool,
    pub persist_quarantine: bool,
    pub now_unix: u64,
    pub quarantined: Vec<QuarantinedRow>,
    pub delete_by_rowid: Vec<(&'static str, i64)>,
}

impl LoadContext {
    pub fn new(quarantine_unreadable: bool) -> Self {
        Self {
            quarantine_unreadable,
            persist_quarantine: true,
            now_unix: crate::clock::host_wall_unix_secs_and_nanos().0,
            quarantined: Vec::new(),
            delete_by_rowid: Vec::new(),
        }
    }

    pub fn quarantine_in_memory() -> Self {
        Self {
            quarantine_unreadable: true,
            persist_quarantine: false,
            now_unix: crate::clock::host_wall_unix_secs_and_nanos().0,
            quarantined: Vec::new(),
            delete_by_rowid: Vec::new(),
        }
    }

    pub fn record_bad_row(
        &mut self,
        source_table: &'static str,
        table_display: &str,
        key: impl Into<String>,
        payload: impl Into<String>,
        error: impl Into<String>,
        rowid: Option<i64>,
    ) -> Result<(), String> {
        let key_str = key.into();
        let err_str = error.into();
        if !self.quarantine_unreadable {
            return Err(format!(
                "Failed to load row from table '{table_display}' (key '{key_str}'): {err_str}"
            ));
        }
        let payload_str = payload.into();
        if self.persist_quarantine {
            tracing::error!(
                table = %source_table,
                key = %key_str,
                error = %err_str,
                "Quarantined unreadable SQLite row into quarantine_rows"
            );
        }
        self.quarantined.push(QuarantinedRow {
            source_table: source_table.to_string(),
            key: key_str,
            payload: payload_str,
            error: err_str,
            quarantined_unix: self.now_unix,
        });
        if let Some(rid) = rowid {
            self.delete_by_rowid.push((source_table, rid));
        }
        Ok(())
    }

    pub fn flush_quarantined_rows(&self, conn: &Connection) -> Result<(), String> {
        if !self.persist_quarantine || self.quarantined.is_empty() || !conn.is_autocommit() {
            return Ok(());
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS quarantine_rows (
                source_table TEXT NOT NULL,
                key BLOB NOT NULL,
                payload BLOB NOT NULL,
                error TEXT NOT NULL,
                quarantined_unix INTEGER NOT NULL
            );",
        )
        .map_err(|e| e.to_string())?;
        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        for q in &self.quarantined {
            tx.execute(
                "INSERT INTO quarantine_rows (source_table, key, payload, error, quarantined_unix) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    q.source_table,
                    q.key,
                    q.payload,
                    q.error,
                    q.quarantined_unix as i64
                ],
            )
            .map_err(|e| e.to_string())?;
        }
        for &(table, rid) in &self.delete_by_rowid {
            let sql = format!("DELETE FROM {table} WHERE rowid = ?1");
            let _ = tx.execute(&sql, params![rid]);
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }
}

pub fn value_ref_to_string(v: ValueRef<'_>) -> String {
    match v {
        ValueRef::Null => "NULL".to_string(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => f.to_string(),
        ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
        ValueRef::Blob(b) => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => {
                let mut hex = String::with_capacity(2 + b.len() * 2);
                hex.push_str("0x");
                for byte in b {
                    use std::fmt::Write as _;
                    let _ = write!(&mut hex, "{byte:02x}");
                }
                hex
            }
        },
    }
}

pub fn row_col_string(row: &Row<'_>, col: usize) -> String {
    row.get_ref(col)
        .map(value_ref_to_string)
        .unwrap_or_else(|_| "<missing>".to_string())
}

pub fn row_composite_key_string(row: &Row<'_>, col1: usize, col2: usize) -> String {
    format!("{}:{}", row_col_string(row, col1), row_col_string(row, col2))
}

pub fn row_payload_summary(row: &Row<'_>, start_col: usize, end_col: usize) -> String {
    let mut parts = Vec::with_capacity(end_col.saturating_sub(start_col));
    for c in start_col..end_col {
        parts.push(row_col_string(row, c));
    }
    parts.join("|")
}

/// Reads all rows currently stored in `quarantine_rows`.
pub fn list_quarantined_rows(conn: &Connection) -> Result<Vec<QuarantinedRow>, String> {
    let mut stmt = match conn.prepare(
        "SELECT source_table, key, payload, error, quarantined_unix FROM quarantine_rows ORDER BY rowid ASC",
    ) {
        Ok(s) => s,
        Err(_) => return Ok(Vec::new()),
    };
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().map_err(|e| e.to_string())? {
        out.push(QuarantinedRow {
            source_table: row_col_string(r, 0),
            key: row_col_string(r, 1),
            payload: row_col_string(r, 2),
            error: row_col_string(r, 3),
            quarantined_unix: r.get::<_, i64>(4).unwrap_or(0).max(0) as u64,
        });
    }
    Ok(out)
}
