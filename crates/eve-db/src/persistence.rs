//! Durable transactional snapshot persistence, WAL journaling, and SQLite relational storage.

use crate::sqlite_store;
use crate::types::AuthRuntimeState;
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::RwLock;

pub(crate) const WAL_CHECKPOINT_INTERVAL: usize = 64;

/// Incremental Write-Ahead Log (WAL) delta record for hot-path mutations (`OF-06` / `OF-23` / `R21-P1-5`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldStateWalDelta {
    #[serde(default)]
    pub(crate) seq: u64,
    pub(crate) next_entity_id: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_characters: Vec<CharacterRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_items: Vec<InventoryItemRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) removed_items: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_market_orders: Vec<MarketOrderRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) removed_market_orders: Vec<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) appended_wallet_journal: Vec<WalletJournalEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_contracts: Vec<ContractRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_system_states: Vec<SystemDynamicState>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) added_idempotency_keys: Vec<(i64, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) full_snapshot: Option<WorldStateSnapshot>,
}

impl WorldStateWalDelta {
    #[cfg(test)]
    pub(crate) fn from_diff(old: &WorldStateSnapshot, new: &WorldStateSnapshot) -> Self {
        let cold_equal = old.markets_seeded == new.markets_seeded
            && old.accounts == new.accounts
            && old.skills == new.skills
            && old.skill_queues == new.skill_queues
            && old.jump_clones == new.jump_clones
            && old.active_implants == new.active_implants
            && old.saved_fittings == new.saved_fittings
            && old.industry_jobs == new.industry_jobs
            && old.planetary_colonies == new.planetary_colonies
            && old.active_missions == new.active_missions
            && old.mission_tally == new.mission_tally
            && old.standing_transactions == new.standing_transactions
            && old.npe_progress == new.npe_progress
            && old.bookmarks == new.bookmarks
            && old.mails == new.mails
            && old.fleets == new.fleets
            && old.corporations == new.corporations
            && old.alliances == new.alliances
            && old.wars == new.wars
            && old.corp_projects == new.corp_projects
            && old.killmails == new.killmails
            && old.kill_rights == new.kill_rights
            && old.structures == new.structures
            && old.licensed_skins == new.licensed_skins
            && old.applied_ship_skins == new.applied_ship_skins
            && old.module_online_states == new.module_online_states
            && old.module_active_states == new.module_active_states
            && old.module_active_charges == new.module_active_charges
            && old.weapon_banks == new.weapon_banks
            && old.asteroids == new.asteroids
            && old.owner_notes == new.owner_notes
            && old.paused_skill_queues == new.paused_skill_queues
            && old.character_last_jump_unix == new.character_last_jump_unix
            && old.character_cspa_charges == new.character_cspa_charges
            && old.mail_timestamps == new.mail_timestamps
            && old.character_login_stats == new.character_login_stats
            && old.item_custom_names == new.item_custom_names
            && old.item_positions == new.item_positions
            && old.structure_vulnerability_windows == new.structure_vulnerability_windows
            && old.skill_history == new.skill_history
            && old.character_doll_info == new.character_doll_info
            && old.wallet_journal_meta == new.wallet_journal_meta
            && old.market_transactions == new.market_transactions
            && old.account_plex_vault == new.account_plex_vault
            && old.corp_lp_balances == new.corp_lp_balances
            && old.char_lp_logs == new.char_lp_logs
            && old.next_alliance_id == new.next_alliance_id
            && old.next_corporation_id == new.next_corporation_id
            && old.next_notification_id == new.next_notification_id
            && old.next_report_id == new.next_report_id
            && new.wallet_journal.len() >= old.wallet_journal.len()
            && new.wallet_journal[..old.wallet_journal.len()] == old.wallet_journal[..]
            && old
                .characters
                .keys()
                .all(|k| new.characters.contains_key(k))
            && old.contracts.keys().all(|k| new.contracts.contains_key(k))
            && old
                .system_states
                .keys()
                .all(|k| new.system_states.contains_key(k))
            && old
                .committed_idempotency_keys
                .iter()
                .all(|k| new.committed_idempotency_keys.contains(k));

        if !cold_equal {
            return Self {
                seq: new.wal_seq,
                next_entity_id: new.next_entity_id,
                full_snapshot: Some(new.clone()),
                ..Default::default()
            };
        }

        let mut delta = Self {
            seq: new.wal_seq,
            next_entity_id: new.next_entity_id,
            ..Default::default()
        };
        for (id, ch) in &new.characters {
            if old.characters.get(id) != Some(ch) {
                delta.upserted_characters.push(ch.clone());
            }
        }
        for (id, item) in &new.items {
            if old.items.get(id) != Some(item) {
                delta.upserted_items.push(item.clone());
            }
        }
        for id in old.items.keys() {
            if !new.items.contains_key(id) {
                delta.removed_items.push(*id);
            }
        }
        for (id, ord) in &new.market_orders {
            if old.market_orders.get(id) != Some(ord) {
                delta.upserted_market_orders.push(ord.clone());
            }
        }
        for id in old.market_orders.keys() {
            if !new.market_orders.contains_key(id) {
                delta.removed_market_orders.push(*id);
            }
        }
        if new.wallet_journal.len() > old.wallet_journal.len() {
            delta
                .appended_wallet_journal
                .extend_from_slice(&new.wallet_journal[old.wallet_journal.len()..]);
        }
        for (id, c) in &new.contracts {
            if old.contracts.get(id) != Some(c) {
                delta.upserted_contracts.push(c.clone());
            }
        }
        for (id, st) in &new.system_states {
            if old.system_states.get(id) != Some(st) {
                delta.upserted_system_states.push(st.clone());
            }
        }
        for k in &new.committed_idempotency_keys {
            if !old.committed_idempotency_keys.contains(k) {
                delta.added_idempotency_keys.push(k.clone());
            }
        }
        delta
    }

    fn apply_to(self, snap: &mut WorldStateSnapshot) {
        if let Some(mut full) = self.full_snapshot {
            full.wal_seq = full.wal_seq.max(self.seq);
            *snap = full;
            return;
        }
        snap.wal_seq = snap.wal_seq.max(self.seq);
        snap.next_entity_id = self.next_entity_id;
        for ch in self.upserted_characters {
            snap.characters.insert(ch.character_id, ch);
        }
        for id in self.removed_items {
            snap.items.remove(&id);
        }
        for item in self.upserted_items {
            snap.items.insert(item.item_id, item);
        }
        for id in self.removed_market_orders {
            snap.market_orders.remove(&id);
        }
        for ord in self.upserted_market_orders {
            snap.market_orders.insert(ord.order_id, ord);
        }
        snap.wallet_journal.extend(self.appended_wallet_journal);
        for c in self.upserted_contracts {
            snap.contracts.insert(c.contract_id, c);
        }
        for st in self.upserted_system_states {
            snap.system_states.insert(st.solar_system_id, st);
        }
        for k in self.added_idempotency_keys {
            snap.committed_idempotency_keys.insert(k);
        }
    }
}

/// Encapsulated transactional world repository backed by SQLite (`D-3`, `D-4`).
pub struct WorldRepository {
    pub(crate) inner: RwLock<WorldStateSnapshot>,
    pub(crate) sqlite_conn: std::sync::Mutex<rusqlite::Connection>,
    pub(crate) sqlite_fp: std::sync::Mutex<sqlite_store::TableFingerprints>,
    pub(crate) sqlite_path: Option<std::path::PathBuf>,
    pub(crate) auth_runtime: RwLock<AuthRuntimeState>,
    pub(crate) clock: std::sync::RwLock<std::sync::Arc<dyn crate::clock::ServerClock>>,
    pub(crate) wal_lock: std::sync::Mutex<()>,
    pub(crate) snapshot_path: Option<std::path::PathBuf>,
    pub(crate) pending_wal_records: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    pub(crate) wal_fault_mode: std::sync::atomic::AtomicU8,
}

impl Drop for WorldRepository {
    fn drop(&mut self) {
        if let Some(ref p) = self.snapshot_path {
            if self
                .pending_wal_records
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
            {
                let _ = self.save_snapshot_to_file(p);
            }
        }
    }
}

impl Default for WorldRepository {
    fn default() -> Self {
        Self::new_empty()
    }
}

impl WorldRepository {
    /// Returns `true` when this repository is an ephemeral `:memory:` instance with no backing SQLite or snapshot file.
    pub fn is_ephemeral_in_memory(&self) -> bool {
        self.sqlite_path.is_none() && self.snapshot_path.is_none()
    }

    /// Returns the shared [`crate::clock::ServerClock`] for this repository (`T-7`).
    pub fn clock(&self) -> std::sync::Arc<dyn crate::clock::ServerClock> {
        self.clock
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Replaces the [`crate::clock::ServerClock`] used by this repository (`T-7`).
    pub fn set_clock(&self, clock: std::sync::Arc<dyn crate::clock::ServerClock>) {
        *self.clock.write().unwrap_or_else(|e| e.into_inner()) = clock;
    }

    /// Returns the current server Unix timestamp in seconds from the injected [`crate::clock::ServerClock`] (`T-7`).
    pub fn now_unix_secs(&self) -> u64 {
        self.clock().now_unix_secs()
    }

    pub(crate) fn wal_path_for(path: &Path) -> std::path::PathBuf {
        path.with_extension("json.wal")
    }

    fn is_sqlite_extension(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("sqlite" | "sqlite3" | "db")
        )
    }

    /// Opens or initializes a durable relational SQLite world database at `path`, applies schema
    /// migrations (`schema_migrations`), and loads the persisted world state (`D-3`).
    pub fn open_sqlite(path: impl AsRef<Path>) -> Result<Self, String> {
        let target = path.as_ref();
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let conn = rusqlite::Connection::open(target).map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let (snap, fp) = sqlite_store::load_snapshot_from_sqlite(&conn)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;
        Ok(Self {
            inner: RwLock::new(snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: Some(target.to_path_buf()),
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            clock: std::sync::RwLock::new(std::sync::Arc::new(
                crate::clock::VirtualServerClock::new(),
            )),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        })
    }

    /// Returns the file path of the backing SQLite database if opened via [`Self::open_sqlite`].
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.sqlite_path.as_deref()
    }

    /// Creates a production `WorldRepository` with an empty account, character, and player item table
    /// prior to loading `EVOMU_DB_PATH` or `EVOMU_SNAPSHOT_PATH` (`R22-P1-5`, `R23-P0-1`, `R24-P1-1`, `D-3`).
    pub fn new_empty() -> Self {
        let db_or_snapshot_path = std::env::var("EVOMU_DB_PATH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                std::env::var("EVOMU_SNAPSHOT_PATH")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
            })
            .map(std::path::PathBuf::from);
        Self::new_empty_with_snapshot_path(db_or_snapshot_path)
            .expect("Failed to load or initialize persisted WorldRepository database")
    }

    /// Creates a production `WorldRepository` with an empty player state, then loads `snapshot_path`
    /// if it already exists on disk without clearing any loaded accounts, characters, skills, or items.
    /// When `snapshot_path` has a `.sqlite`/`.sqlite3`/`.db` extension, opens the relational SQLite
    /// store directly via [`Self::open_sqlite`] (`D-3`).
    /// Returns `Err` if loading or initializing fails, refusing to start with an empty world over an
    /// unreadable save (`R23-P0-1`, `R24-P1-1`, `S-5`, `D-3`).
    pub fn new_empty_with_snapshot_path(
        snapshot_path: Option<std::path::PathBuf>,
    ) -> Result<Self, String> {
        if let Some(ref p) = snapshot_path {
            if Self::is_sqlite_extension(p) {
                return Self::open_sqlite(p);
            }
        }

        let market_orders = HashMap::new();
        let system_states = HashMap::new();
        let initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts: HashMap::new(),
            characters: HashMap::new(),
            skills: HashMap::new(),
            skill_queues: HashMap::new(),
            jump_clones: HashMap::new(),
            active_implants: HashMap::new(),
            items: HashMap::new(),
            saved_fittings: HashMap::new(),
            market_orders,
            wallet_journal: Vec::new(),
            contracts: HashMap::new(),
            industry_jobs: HashMap::new(),
            planetary_colonies: HashMap::new(),
            active_missions: HashMap::new(),
            mission_tally: HashMap::new(),
            standing_transactions: HashMap::new(),
            npe_progress: HashMap::new(),
            bookmarks: HashMap::new(),
            mails: Vec::new(),
            fleets: HashMap::new(),
            corporations: HashMap::new(),
            alliances: HashMap::new(),
            wars: HashMap::new(),
            corp_projects: HashMap::new(),
            killmails: HashMap::new(),
            kill_rights: HashMap::new(),
            structures: HashMap::new(),
            system_states,
            committed_idempotency_keys: HashSet::new(),
            licensed_skins: HashMap::new(),
            applied_ship_skins: HashMap::new(),
            chat_channels: HashMap::new(),
            cosmic_signatures: HashMap::new(),
            read_mails: HashSet::new(),
            deleted_mails: HashSet::new(),
            mail_labels: HashMap::new(),
            mail_assigned_labels: HashMap::new(),
            bookmark_folders: HashMap::new(),
            wh_polarizations: HashMap::new(),
            mining_ledger: Vec::new(),
            module_online_states: HashMap::new(),
            module_active_states: HashSet::new(),
            module_active_charges: HashMap::new(),
            weapon_banks: HashMap::new(),
            asteroids: HashMap::new(),
            moon_extractions: HashMap::new(),
            auto_moon_miners: HashMap::new(),
            observer_mining_ledger: HashMap::new(),
            owner_notes: HashMap::new(),
            paused_skill_queues: HashSet::new(),
            cosmic_anomalies: HashMap::new(),
            sov_prospecting_levels: HashMap::new(),
            last_mission_decline_times: HashMap::new(),
            character_last_jump_unix: HashMap::new(),
            character_cspa_charges: HashMap::new(),
            mail_timestamps: HashMap::new(),
            character_login_stats: HashMap::new(),
            item_custom_names: HashMap::new(),
            item_positions: HashMap::new(),
            structure_vulnerability_windows: HashMap::new(),
            skill_history: HashMap::new(),
            character_doll_info: HashMap::new(),
            ..Default::default()
        };
        let mut conn = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;

        let mut repo = Self {
            inner: RwLock::new(initial_snap),
            sqlite_conn: std::sync::Mutex::new(conn),
            sqlite_fp: std::sync::Mutex::new(fp),
            sqlite_path: None,
            auth_runtime: RwLock::new(AuthRuntimeState::new(jwt_secret)),
            clock: std::sync::RwLock::new(std::sync::Arc::new(
                crate::clock::VirtualServerClock::new(),
            )),
            wal_lock: std::sync::Mutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(test)]
            wal_fault_mode: std::sync::atomic::AtomicU8::new(0),
        };

        if let Some(ref p) = snapshot_path {
            if p.exists() {
                repo.load_snapshot_from_file(p)?;
            } else {
                repo.save_snapshot_to_file(p)?;
            }
            repo.snapshot_path = snapshot_path;
        }
        Ok(repo)
    }
    /// Serializes and atomically flushes the in-memory world snapshot to disk.
    fn save_snapshot_locked(&self, target: &Path, snap: &WorldStateSnapshot) -> Result<(), String> {
        use std::io::Write as _;
        let bytes = serde_json::to_vec(snap).map_err(|e| e.to_string())?;
        let tmp_path = target.with_extension("json.tmp");
        {
            let mut tmp_file = std::fs::File::create(&tmp_path).map_err(|e| e.to_string())?;
            tmp_file.write_all(&bytes).map_err(|e| e.to_string())?;
            tmp_file.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp_path, target).map_err(|e| e.to_string())?;
        let wal_path = Self::wal_path_for(target);
        if wal_path.exists() {
            let wal_tmp = target.with_extension("json.wal.tmp");
            if let Ok(empty_wal) = std::fs::File::create(&wal_tmp) {
                let _ = empty_wal.sync_all();
                let _ = std::fs::rename(&wal_tmp, &wal_path);
            }
            let _ = std::fs::remove_file(&wal_path);
        }
        self.pending_wal_records
            .store(0, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Saves a compact checkpoint of the complete transactional world state to a JSON snapshot file
    /// (`M1` durable persistence) under `inner` + `wal_lock`, using an atomic temp-file write + `fsync` +
    /// rename and atomic `.json.wal` checkpoint rotation (`R21-P1-5`).
    pub fn save_snapshot_to_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.save_snapshot_locked(path.as_ref(), &guard)
    }

    /// Appends a single compact JSONL delta record to `<path>.json.wal` with `sync_data()` (`fsync`)
    /// while holding `inner` and `wal_lock`, truncating any uncleaned partial tail before appending,
    /// performing best-effort `set_len(prev_len)` + `sync_all()` rollback on write/sync error, and
    /// treating periodic post-sync checkpoint compaction as retryable (`R21-P1-5`, `R25-P1-1`).
    pub(crate) fn append_wal_delta_locked(
        &self,
        path: &Path,
        snap: &WorldStateSnapshot,
        delta: &WorldStateWalDelta,
    ) -> Result<(), String> {
        use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
        if !path.exists() {
            return self.save_snapshot_locked(path, snap);
        }
        let wal_path = Self::wal_path_for(path);
        let mut line = serde_json::to_vec(delta).map_err(|e| e.to_string())?;
        line.push(b'\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&wal_path)
            .map_err(|e| e.to_string())?;

        // Find the byte offset of the last newline boundary so any uncleaned partial write at the
        // tail of `.json.wal` is truncated before appending without re-parsing prior records (`R25-P1-1`).
        let mut existing = Vec::new();
        file.read_to_end(&mut existing).map_err(|e| e.to_string())?;
        let prev_len: u64 = if existing.ends_with(b"\n") {
            existing.len() as u64
        } else {
            existing
                .iter()
                .rposition(|&b| b == b'\n')
                .map(|idx| (idx + 1) as u64)
                .unwrap_or(0)
        };
        if existing.len() as u64 != prev_len {
            file.set_len(prev_len).map_err(|e| e.to_string())?;
            file.sync_all().map_err(|e| e.to_string())?;
        }
        file.seek(SeekFrom::Start(prev_len))
            .map_err(|e| e.to_string())?;

        #[cfg(test)]
        let fault_mode = self
            .wal_fault_mode
            .swap(0, std::sync::atomic::Ordering::SeqCst);
        let write_and_sync = (|| -> Result<(), String> {
            #[cfg(test)]
            if fault_mode == 1 {
                let half = (line.len() / 2).max(1);
                file.write_all(&line[..half]).map_err(|e| e.to_string())?;
                return Err("Injected partial WAL write_all failure".to_string());
            }
            file.write_all(&line).map_err(|e| e.to_string())?;
            #[cfg(test)]
            if fault_mode == 2 {
                return Err("Injected WAL sync_data failure after write_all".to_string());
            }
            file.sync_data().map_err(|e| e.to_string())?;
            Ok(())
        })();

        if let Err(mut err) = write_and_sync {
            if let Err(trunc_err) = file.set_len(prev_len).and_then(|()| file.sync_all()) {
                err = format!("{err}; rollback set_len/sync_all also failed: {trunc_err}");
            }
            return Err(err);
        }

        let count = self
            .pending_wal_records
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if count >= WAL_CHECKPOINT_INTERVAL && count % WAL_CHECKPOINT_INTERVAL == 0 {
            // Once the WAL line is durably written and synced, treat checkpoint compaction as a
            // retryable background operation once per interval (`R25-P1-1`).
            let _ = self.save_snapshot_locked(path, snap);
        }
        Ok(())
    }

    /// Restores the transactional world state from a JSON snapshot file and replays any
    /// uncompacted incremental `.json.wal` delta records in strict monotonic `seq` order,
    /// tolerating and healing a truncated/torn trailing WAL record from a prior crash and
    /// rejecting duplicate sequence numbers (`R21-P1-5`, `R24-P1-1`, `R25-P1-1`).
    pub fn load_snapshot_from_file(&self, path: impl AsRef<Path>) -> Result<(), String> {
        use std::io::Write as _;
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut fp = self.sqlite_fp.lock().unwrap_or_else(|e| e.into_inner());
        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
        let target = path.as_ref();
        let json = std::fs::read_to_string(target).map_err(|e| e.to_string())?;
        let mut snap: WorldStateSnapshot =
            serde_json::from_str(&json).map_err(|e| e.to_string())?;
        let wal_path = Self::wal_path_for(target);
        if wal_path.exists() {
            let wal_text = std::fs::read_to_string(&wal_path).map_err(|e| e.to_string())?;
            let non_empty_lines: Vec<&str> = wal_text
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            let mut deltas: Vec<WorldStateWalDelta> = Vec::new();
            let mut seen_seqs: HashSet<u64> = HashSet::new();
            let mut had_torn_tail = false;
            for (idx, line) in non_empty_lines.iter().enumerate() {
                match serde_json::from_str::<WorldStateWalDelta>(line) {
                    Ok(delta) => {
                        if delta.seq == 0 || !seen_seqs.insert(delta.seq) {
                            return Err(format!(
                                "Duplicate or invalid WAL record sequence number {} at line {}",
                                delta.seq,
                                idx + 1
                            ));
                        }
                        deltas.push(delta);
                    }
                    Err(e) => {
                        if idx + 1 == non_empty_lines.len() {
                            had_torn_tail = true;
                            break;
                        }
                        return Err(format!(
                            "Corrupt non-trailing WAL record at line {}: {}",
                            idx + 1,
                            e
                        ));
                    }
                }
            }
            if had_torn_tail {
                let wal_tmp = target.with_extension("json.wal.tmp");
                let mut healed = std::fs::File::create(&wal_tmp).map_err(|e| e.to_string())?;
                for d in &deltas {
                    let mut line = serde_json::to_vec(d).map_err(|e| e.to_string())?;
                    line.push(b'\n');
                    healed.write_all(&line).map_err(|e| e.to_string())?;
                }
                healed.sync_all().map_err(|e| e.to_string())?;
                std::fs::rename(&wal_tmp, &wal_path).map_err(|e| e.to_string())?;
            }
            deltas.sort_by_key(|d| d.seq);
            let valid_count = deltas.len();
            for delta in deltas {
                if delta.seq != 0 && delta.seq <= snap.wal_seq {
                    continue;
                }
                delta.apply_to(&mut snap);
            }
            self.pending_wal_records
                .store(valid_count, std::sync::atomic::Ordering::SeqCst);
        }
        snap.fold_legacy_plex_balances();
        snap.reconcile_id_counters();
        *fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &snap)?;
        *guard = snap;
        Ok(())
    }

    pub fn with_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&WorldStateSnapshot) -> R,
    {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        f(&guard)
    }

    /// Delegates infallible snapshot mutations through `try_with_mut_snapshot`.
    pub fn with_mut_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WorldStateSnapshot) -> R,
    {
        match self.try_with_mut_snapshot(|snap| Ok::<R, std::convert::Infallible>(f(snap))) {
            Ok(out) => out,
            Err(infallible) => match infallible {},
        }
    }

    /// Transactionally applies a fallible closure `f` in-place (with zero whole-world `WorldStateSnapshot`
    /// cloning) and commits dirty rows inside an immediate `rusqlite::Transaction` ONLY when `f` returns
    /// `Ok(val)` AND durable persistence succeeds, rolling back in-memory state from the committed SQLite
    /// tables on any error (`D-3`, `R21-P1-5`, `R24-P1-1`).
    pub fn try_with_mut_snapshot<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut WorldStateSnapshot) -> Result<T, E>,
        E: FromWalError,
    {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        let mut conn = self.sqlite_conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut fp = self.sqlite_fp.lock().unwrap_or_else(|e| e.into_inner());
        match f(&mut guard) {
            Ok(val) => {
                guard.wal_seq = guard.wal_seq.saturating_add(1);
                let need_wal_delta = self.snapshot_path.is_some();
                let mut staged_fp = fp.clone();
                let persist_res = (|| -> Result<(), String> {
                    let tx = conn
                        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                        .map_err(|e| e.to_string())?;
                    let delta = sqlite_store::sync_dirty_to_sqlite(
                        &tx,
                        &guard,
                        &mut staged_fp,
                        need_wal_delta,
                    )?;
                    if let Some(ref p) = self.snapshot_path {
                        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
                        self.append_wal_delta_locked(p, &guard, &delta)?;
                    }
                    tx.commit().map_err(|e| e.to_string())?;
                    Ok(())
                })();
                match persist_res {
                    Ok(()) => {
                        *fp = staged_fp;
                        Ok(val)
                    }
                    Err(persist_err) => {
                        sqlite_store::rollback_dirty_from_sqlite(&conn, &mut guard, &fp)
                            .expect("Fatal: failed to roll back dirty state from SQLite after persist error");
                        Err(E::from_wal_error(persist_err))
                    }
                }
            }
            Err(err) => {
                sqlite_store::rollback_dirty_from_sqlite(&conn, &mut guard, &fp).expect(
                    "Fatal: failed to roll back dirty state from SQLite after transaction error",
                );
                Err(err)
            }
        }
    }

    pub fn get_asteroid(&self, asteroid_id: i64) -> Option<AsteroidRecord> {
        self.with_snapshot(|snap| snap.asteroids.get(&asteroid_id).cloned())
    }

    pub fn get_asteroids_in_system(&self, solar_system_id: u32) -> Vec<AsteroidRecord> {
        self.with_snapshot(|snap| {
            snap.asteroids
                .values()
                .filter(|a| a.solar_system_id == solar_system_id)
                .cloned()
                .collect()
        })
    }

    pub fn spawn_asteroid(&self, record: AsteroidRecord) {
        self.with_mut_snapshot(|snap| {
            snap.asteroids.entry(record.asteroid_id).or_insert(record);
        });
    }
}
