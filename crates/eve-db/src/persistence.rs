//! Durable transactional snapshot persistence, WAL journaling, and SQLite relational storage.

use crate::lock_rank::{RankedMutex, RankedRwLock, RANK_CLOCK, RANK_LEAF, RANK_WAL_LOCK, RANK_WORLD_INNER};
use crate::sqlite_store;
use crate::tx_engine::{
    apply_changeset_to_sqlite, validate_changeset, ChangeSet, Effects, EntityIdAllocator,
    PostCommitListener, TxError, TxFaultPlan, TxSqlExecutor, WorldTx, DEFAULT_MAX_ROW_JSON_BYTES,
};
use crate::types::AuthRuntimeState;
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) const WAL_CHECKPOINT_INTERVAL: usize = 64;

/// Incremental Write-Ahead Log (WAL) delta record for hot-path mutations (`OF-06` / `OF-23` / `R21-P1-5`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorldStateWalDelta {
    #[serde(skip)]
    pub(crate) dirty: bool,
    #[serde(default)]
    pub(crate) seq: u64,
    pub(crate) next_entity_id: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_characters: Vec<CharacterRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_skills: Vec<(i64, TrainedSkillRecord)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) upserted_skill_queues: Vec<(i64, Vec<SkillQueueEntry>)>,
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
    pub(crate) full_snapshot: Option<Box<WorldStateSnapshot>>,
}

impl WorldStateWalDelta {
    #[cfg(test)]
    pub(crate) fn from_diff(old: &WorldStateSnapshot, new: &WorldStateSnapshot) -> Self {
        let skills_only_upserted = old.skills.iter().all(|(cid, old_m)| {
            new.skills
                .get(cid)
                .is_some_and(|new_m| old_m.keys().all(|tid| new_m.contains_key(tid)))
        }) && old
            .skill_queues
            .keys()
            .all(|cid| new.skill_queues.contains_key(cid));
        let cold_equal = old.markets_seeded == new.markets_seeded
            && old.accounts == new.accounts
            && skills_only_upserted
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
            && old.read_mails == new.read_mails
            && old.deleted_mails == new.deleted_mails
            && old.trashed_mails == new.trashed_mails
            && old.mail_labels == new.mail_labels
            && old.mail_assigned_labels == new.mail_assigned_labels
            && old.mailing_lists == new.mailing_lists
            && old.calendar_events == new.calendar_events
            && old.bookmark_folders == new.bookmark_folders
            && old.bookmark_subfolders == new.bookmark_subfolders
            && old.bookmark_expiries == new.bookmark_expiries
            && old.bookmark_folder_acls == new.bookmark_folder_acls
            && old.access_groups == new.access_groups
            && old.bookmark_folder_known_by == new.bookmark_folder_known_by
            && old.chat_channels == new.chat_channels
            && old.chat_channel_acl == new.chat_channel_acl
            && old.char_contacts == new.char_contacts
            && old.char_contact_notifications == new.char_contact_notifications
            && old.char_contact_labels == new.char_contact_labels
            && old.corp_contacts == new.corp_contacts
            && old.corp_contact_labels == new.corp_contact_labels
            && old.alliance_contacts == new.alliance_contacts
            && old.alliance_contact_labels == new.alliance_contact_labels
            && old.char_notes == new.char_notes
            && old.moon_extractions == new.moon_extractions
            && old.auto_moon_miners == new.auto_moon_miners
            && old.observer_mining_ledger == new.observer_mining_ledger
            && old.mining_ledger == new.mining_ledger
            && old.char_research_agents == new.char_research_agents
            && old.planetary_colony_layouts == new.planetary_colony_layouts
            && old.corp_customs_offices == new.corp_customs_offices
            && old.orbital_provision_types == new.orbital_provision_types
            && old.wallet_journal_timestamps == new.wallet_journal_timestamps
            && old.char_wallet_transactions == new.char_wallet_transactions
            && old.corp_wallet_journals == new.corp_wallet_journals
            && old.corp_wallet_transactions == new.corp_wallet_transactions
            && old.market_order_meta == new.market_order_meta
            && old.market_history == new.market_history
            && old.cosmic_signatures == new.cosmic_signatures
            && old.cosmic_anomalies == new.cosmic_anomalies
            && old.incursions == new.incursions
            && old.last_mission_decline_times == new.last_mission_decline_times
            && old.wh_polarizations == new.wh_polarizations
            && old.starbase_settings == new.starbase_settings
            && old.corp_container_logs == new.corp_container_logs
            && old.corp_vote_cases == new.corp_vote_cases
            && old.corp_vote_ballots == new.corp_vote_ballots
            && old.corp_governance == new.corp_governance
            && old.service_state_blobs == new.service_state_blobs
            && old.quasar_state_blobs == new.quasar_state_blobs
            && old.sov_prospecting_levels == new.sov_prospecting_levels
            && old.system_jump_events == new.system_jump_events
            && old.system_kill_events == new.system_kill_events
            && old.sov_campaign_scores == new.sov_campaign_scores
            && old.fw_character_stats == new.fw_character_stats
            && old.fw_corp_enlisted_on == new.fw_corp_enlisted_on
            && old.fw_system_occupations == new.fw_system_occupations
            && old.fw_wars == new.fw_wars
            && old.war_lifecycle_meta == new.war_lifecycle_meta
            && old.killmail_details == new.killmail_details
            && old.station_office_rental_costs == new.station_office_rental_costs
            && old.alliance_lifecycle_meta == new.alliance_lifecycle_meta
            && old.corp_hangar_divisions == new.corp_hangar_divisions
            && old.structure_esi_timer_meta == new.structure_esi_timer_meta
            && old.corp_alliance_history == new.corp_alliance_history
            && old.corp_medals == new.corp_medals
            && old.corp_issued_medals == new.corp_issued_medals
            && old.corp_member_limits == new.corp_member_limits
            && old.corp_titles == new.corp_titles
            && old.corp_member_titles == new.corp_member_titles
            && old.corp_role_assignments == new.corp_role_assignments
            && old.corp_role_history == new.corp_role_history
            && old.corp_shareholders == new.corp_shareholders
            && old.corp_project_contributors == new.corp_project_contributors
            && old.char_medals == new.char_medals
            && old.char_scoped_roles == new.char_scoped_roles
            && old.loyalty_store_offers == new.loyalty_store_offers
            && old.employment_history == new.employment_history
            && old.alliance_war_state == new.alliance_war_state
            && old.war_state == new.war_state
            && old.fw_runtime == new.fw_runtime
            && old.sov_runtime == new.sov_runtime
            && old.insurgency_runtime == new.insurgency_runtime
            && old.crimewatch_states == new.crimewatch_states
            && old.crimewatch_aggressions == new.crimewatch_aggressions
            && old.combat_crimewatch_state == new.combat_crimewatch_state
            && old.zarzakh_gate_locks == new.zarzakh_gate_locks
            && old.structure_docking_acl_denied == new.structure_docking_acl_denied
            && old.sp6_lifecycle == new.sp6_lifecycle
            && old.pve_special_sites == new.pve_special_sites
            && old.crab_beacons == new.crab_beacons
            && old.abyssal_state == new.abyssal_state
            && old.dungeon_runtime == new.dungeon_runtime
            && old.dungeon_editor == new.dungeon_editor
            && old.fob_observatory_state == new.fob_observatory_state
            && old.mining_and_ess_state == new.mining_and_ess_state
            && old.pvp_filament_state == new.pvp_filament_state
            && old.tourney_state == new.tourney_state
            && old.deployable_state == new.deployable_state
            && old.superweapon_runtime == new.superweapon_runtime
            && old.insurgency_complex_ops == new.insurgency_complex_ops
            && old.node_graph_state == new.node_graph_state
            && old.seasons_and_campaigns == new.seasons_and_campaigns
            && old.quasar_commerce == new.quasar_commerce
            && old.fleet_and_social_ops == new.fleet_and_social_ops
            && old.cluster_and_runtime == new.cluster_and_runtime
            && old.entity_kv == new.entity_kv
            && old.bookmark_subfolder_parents == new.bookmark_subfolder_parents
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
                dirty: true,
                seq: new.wal_seq,
                next_entity_id: new.next_entity_id,
                full_snapshot: Some(Box::new(new.clone())),
                ..Default::default()
            };
        }

        let mut delta = Self {
            dirty: old != new,
            seq: new.wal_seq,
            next_entity_id: new.next_entity_id,
            ..Default::default()
        };
        for item in old.characters.diff(&new.characters) {
            if let Some(ch) = item.after_val() {
                delta.upserted_characters.push(ch.clone());
            }
        }
        for item in old.skills.diff(&new.skills) {
            let cid = *item.key();
            if let Some(new_m) = item.after_val() {
                let old_m = item.before_val();
                for (tid, sk) in new_m {
                    if old_m.and_then(|m| m.get(tid)) != Some(sk) {
                        delta.upserted_skills.push((cid, sk.clone()));
                    }
                }
            }
        }
        for item in old.skill_queues.diff(&new.skill_queues) {
            if let Some(new_q) = item.after_val() {
                delta.upserted_skill_queues.push((*item.key(), new_q.clone()));
            }
        }
        for item in old.items.diff(&new.items) {
            match item {
                CowDiffItem::Add(_, v) | CowDiffItem::Update { new: (_, v), .. } => {
                    delta.upserted_items.push((**v).clone());
                }
                CowDiffItem::Remove(&id, _) => {
                    delta.removed_items.push(id);
                }
            }
        }
        for item in old.market_orders.diff(&new.market_orders) {
            match item {
                CowDiffItem::Add(_, v) | CowDiffItem::Update { new: (_, v), .. } => {
                    delta.upserted_market_orders.push((**v).clone());
                }
                CowDiffItem::Remove(&id, _) => {
                    delta.removed_market_orders.push(id);
                }
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
                let mut norm = st.clone();
                norm.solar_system_id = *id;
                delta.upserted_system_states.push(norm);
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
            *snap = *full;
            return;
        }
        snap.wal_seq = snap.wal_seq.max(self.seq);
        snap.next_entity_id = self.next_entity_id;
        for ch in self.upserted_characters {
            snap.characters.insert(ch.character_id, ch);
        }
        for (cid, sk) in self.upserted_skills {
            snap.skills.entry(cid).or_default().insert(sk.type_id, sk);
        }
        for (cid, q) in self.upserted_skill_queues {
            snap.skill_queues.insert(cid, q);
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

thread_local! {
    static DB_WRITE_LOCK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct DbWriteLockDepthGuard;

impl DbWriteLockDepthGuard {
    #[inline]
    fn enter() -> Self {
        DB_WRITE_LOCK_DEPTH.with(|d| d.set(d.get().saturating_add(1)));
        Self
    }
}

impl Drop for DbWriteLockDepthGuard {
    #[inline]
    fn drop(&mut self) {
        DB_WRITE_LOCK_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

static NEXT_REPO_INSTANCE_ID: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

static PERSIST_FAILURES_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

static PERSIST_TX_PANICS_TOTAL: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// Returns the cumulative count of durable SQLite/WAL persistence or rollback failures (`#903` / `#833`).
pub fn persist_failures_total() -> u64 {
    PERSIST_FAILURES_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

/// Returns the cumulative count of panics caught inside `run_mut_snapshot_tx` write closures (`#904` / `#833`).
pub fn tx_panics_total() -> u64 {
    PERSIST_TX_PANICS_TOTAL.load(std::sync::atomic::Ordering::Relaxed)
}

fn panic_payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

pub(crate) struct WorldWriter {
    pub(crate) working: WorldStateSnapshot,
    pub(crate) conn: rusqlite::Connection,
    pub(crate) fp: sqlite_store::TableFingerprints,
    pub(crate) seq: u64,
}

/// Encapsulated transactional world repository backed by SQLite (`D-3`, `D-4`, `RFC-2 §3.2`).
pub struct WorldRepository {
    pub(crate) instance_id: u64,
    pub(crate) published: std::sync::RwLock<std::sync::Arc<WorldStateSnapshot>>,
    pub(crate) writer: RankedMutex<WorldWriter, RANK_WORLD_INNER>,
    pub(crate) entity_ids: std::sync::Arc<EntityIdAllocator>,
    pub(crate) sqlite_path: Option<std::path::PathBuf>,
    pub(crate) auth_runtime: RankedRwLock<AuthRuntimeState, RANK_LEAF>,
    pub(crate) clock: RankedRwLock<std::sync::Arc<dyn crate::clock::ServerClock>, RANK_CLOCK>,
    pub(crate) wal_lock: RankedMutex<(), RANK_WAL_LOCK>,
    pub(crate) snapshot_path: Option<std::path::PathBuf>,
    pub(crate) pending_wal_records: std::sync::atomic::AtomicUsize,
    pub(crate) state_epoch: std::sync::atomic::AtomicU64,
    pub(crate) combat_state_epoch: std::sync::atomic::AtomicU64,
    pub(crate) tx_commit_count: std::sync::atomic::AtomicU64,
    pub(crate) post_commit_listeners: std::sync::Mutex<Vec<(u64, PostCommitListener)>>,
    pub(crate) next_listener_id: std::sync::atomic::AtomicU64,
    pub(crate) fault_plan: std::sync::atomic::AtomicU64,
}

impl Drop for WorldRepository {
    fn drop(&mut self) {
        if let Some(ref p) = self.snapshot_path {
            if self
                .pending_wal_records
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
            {
                let working = self.writer.get_mut().map(|w| w.working.clone());
                if let Ok(snap) = working {
                    let _ = self.save_snapshot_locked(p, &snap);
                }
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
    pub(crate) fn from_parts(
        working: WorldStateSnapshot,
        conn: rusqlite::Connection,
        fp: sqlite_store::TableFingerprints,
        entity_block: i64,
        sqlite_path: Option<std::path::PathBuf>,
        auth_runtime: AuthRuntimeState,
    ) -> Self {
        let floor = entity_block.max(working.next_entity_id).max(200_000_000);
        let initial_seq = working.wal_seq;
        let published = std::sync::Arc::new(working.clone());
        Self {
            instance_id: Self::next_instance_id(),
            published: std::sync::RwLock::new(published),
            writer: RankedMutex::new(WorldWriter {
                working,
                conn,
                fp,
                seq: initial_seq,
            }),
            entity_ids: std::sync::Arc::new(EntityIdAllocator::new(floor, entity_block)),
            sqlite_path,
            auth_runtime: RankedRwLock::new(auth_runtime),
            clock: RankedRwLock::new(std::sync::Arc::new(
                crate::clock::VirtualServerClock::new(),
            )),
            wal_lock: RankedMutex::new(()),
            snapshot_path: None,
            pending_wal_records: std::sync::atomic::AtomicUsize::new(0),
            state_epoch: std::sync::atomic::AtomicU64::new(1),
            combat_state_epoch: std::sync::atomic::AtomicU64::new(1),
            tx_commit_count: std::sync::atomic::AtomicU64::new(0),
            post_commit_listeners: std::sync::Mutex::new(Vec::new()),
            next_listener_id: std::sync::atomic::AtomicU64::new(1),
            fault_plan: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub(crate) fn next_instance_id() -> u64 {
        NEXT_REPO_INSTANCE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Returns the process-unique monotonic instance ID of this [`WorldRepository`].
    pub fn instance_id(&self) -> u64 {
        self.instance_id
    }

    /// Returns a monotonic state epoch incremented on every snapshot commit or load (`#592`).
    pub fn state_epoch(&self) -> u64 {
        self.state_epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Returns a monotonic epoch incremented whenever persisted crimewatch/combat runtime state
    /// changes on commit or when a snapshot load replaces state (`#592`).
    pub fn combat_state_epoch(&self) -> u64 {
        self.combat_state_epoch.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Returns the total number of non-empty transactions committed by [`Self::tx`].
    pub fn tx_commit_count(&self) -> u64 {
        self.tx_commit_count.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Registers a post-commit listener invoked inside the writer critical section in commit order.
    pub fn register_post_commit_listener(&self, listener: PostCommitListener) -> u64 {
        let id = self
            .next_listener_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut guard = self
            .post_commit_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.push((id, listener));
        id
    }

    /// Unregisters a previously registered post-commit listener.
    pub fn remove_post_commit_listener(&self, id: u64) {
        let mut guard = self
            .post_commit_listeners
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        guard.retain(|(entry_id, _)| *entry_id != id);
    }

    /// Returns a reference to the external block-persisted [`EntityIdAllocator`].
    pub fn entity_id_allocator(&self) -> &EntityIdAllocator {
        &self.entity_ids
    }

    /// Runs `f` with a shared reference to the backing SQLite connection if the writer mutex is uncontended.
    pub fn try_with_sqlite_conn<R>(&self, f: impl FnOnce(&rusqlite::Connection) -> R) -> Option<R> {
        self.writer.try_lock().ok().map(|w| f(&w.conn))
    }

    /// Runs `f` with exclusive access to the backing SQLite connection under the writer lock.
    pub fn with_sqlite_conn<R>(&self, f: impl FnOnce(&mut rusqlite::Connection) -> R) -> R {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut writer.conn)
    }

    /// Configures a single-use [`TxFaultPlan`] injected on the next non-empty commit (`#915`).
    #[doc(hidden)]
    pub fn set_fault_plan_for_test(&self, plan: TxFaultPlan) {
        self.fault_plan
            .store(plan.encode(), std::sync::atomic::Ordering::SeqCst);
    }

    #[inline]
    pub(crate) fn bump_state_epoch(&self) {
        self.state_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    #[inline]
    pub(crate) fn bump_combat_state_epoch(&self) {
        self.combat_state_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

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
        Self::open_sqlite_with_options(path, sqlite_store::default_quarantine_unreadable())
    }

    /// Opens or initializes a durable relational SQLite world database at `path` with explicit
    /// `quarantine_unreadable` behavior (`#906` / `#856`).
    ///
    /// When `quarantine_unreadable` is `false` (the default), any unreadable or corrupt row in any
    /// SQLite table fails closed with an error naming the table and primary key. When `true`,
    /// unreadable rows are moved into `quarantine_rows` inside a transaction and deleted from the
    /// source table before startup proceeds.
    pub fn open_sqlite_with_options(
        path: impl AsRef<Path>,
        quarantine_unreadable: bool,
    ) -> Result<Self, String> {
        let target = path.as_ref();
        if let Some(parent) = target.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
        }
        let conn = rusqlite::Connection::open(target).map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let (snap, fp) =
            sqlite_store::load_snapshot_from_sqlite_with_options(&conn, quarantine_unreadable)?;
        let entity_block = sqlite_store::load_entity_block_from_sqlite(&conn)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;
        let auth_runtime = sqlite_store::load_auth_runtime_from_sqlite(&conn, jwt_secret)?;
        Ok(Self::from_parts(
            snap,
            conn,
            fp,
            entity_block,
            Some(target.to_path_buf()),
            auth_runtime,
        ))
    }

    /// Returns all rows quarantined into `quarantine_rows` in the backing SQLite database (`#906`).
    pub fn quarantined_rows(&self) -> Result<Vec<sqlite_store::QuarantinedRow>, String> {
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        sqlite_store::list_quarantined_rows(&writer.conn)
    }

    /// Returns the file path of the backing SQLite database if opened via [`Self::open_sqlite`].
    pub fn sqlite_path(&self) -> Option<&Path> {
        self.sqlite_path.as_deref()
    }

    /// Saves the current snapshot state directly into a SQLite database at `path`, including
    /// durable `AuthRuntimeState` (`jwt_signing_secret`, `revoked_jtis`, `kicked_users`, and
    /// SHA-256-digested `refresh_tokens`) (`#540`).
    pub fn save_to_sqlite(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let repo = Self::open_sqlite(path)?;
        let snap = self.read_state();
        let auth_rt = self.auth_runtime.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut writer = repo.writer.lock().unwrap_or_else(|e| e.into_inner());
        sqlite_store::replace_sqlite_from_snapshot(&mut writer.conn, &snap)?;
        sqlite_store::save_jwt_secret(&writer.conn, &auth_rt.jwt_signing_secret)?;
        sqlite_store::replace_auth_runtime_in_sqlite(&mut writer.conn, &auth_rt)?;
        Ok(())
    }

    /// Prunes expired `revoked_jtis` and expired `refresh_tokens` from both in-memory
    /// `AuthRuntimeState` and SQLite storage relative to `now_unix` (`#540`).
    pub fn prune_expired_auth_runtime(&self, now_unix: u64) -> usize {
        let (mem_pruned, cutoff) = {
            let mut rt = self.auth_runtime.write().unwrap_or_else(|e| e.into_inner());
            let mem_pruned = sqlite_store::prune_auth_runtime_locked(&mut rt, now_unix);
            (mem_pruned, rt.last_auth_now_unix)
        };
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _ = sqlite_store::persist_prune_expired(&mut writer.conn, cutoff);
        mem_pruned
    }

    /// Creates a production `WorldRepository` with an empty account, character, and player item table
    /// prior to loading `EVOMU_DB_PATH` or `EVOMU_SNAPSHOT_PATH` (`R22-P1-5`, `R23-P0-1`, `R24-P1-1`, `D-3`).
    pub fn new_empty() -> Self {
        let db_or_snapshot_path = {
            let _env_guard = crate::lock_test_env_read();
            std::env::var("EVOMU_DB_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    std::env::var("EVOMU_SNAPSHOT_PATH")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                })
                .map(std::path::PathBuf::from)
        };
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

        let market_orders = CowMap::new();
        let system_states = HashMap::new();
        let mut initial_snap = WorldStateSnapshot {
            next_entity_id: 200_000_000,
            wal_seq: 0,
            markets_seeded: false,
            accounts: HashMap::new(),
            characters: CowMap::new(),
            skills: CowMap::new(),
            skill_queues: CowMap::new(),
            jump_clones: HashMap::new(),
            active_implants: HashMap::new(),
            items: CowMap::new(),
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
        initial_snap.fold_legacy_plex_balances();
        initial_snap.reconcile_id_counters();
        initial_snap.backfill_fw_system_occupations();
        let mut conn = rusqlite::Connection::open_in_memory().map_err(|e| e.to_string())?;
        sqlite_store::apply_migrations(&conn)?;
        let fp = sqlite_store::replace_sqlite_from_snapshot(&mut conn, &initial_snap)?;
        let jwt_secret =
            sqlite_store::load_or_init_jwt_secret(&conn, Self::generate_random_secret_32)?;
        let entity_block = initial_snap.next_entity_id.max(200_000_000);

        let mut repo = Self::from_parts(
            initial_snap,
            conn,
            fp,
            entity_block,
            None,
            AuthRuntimeState::new(jwt_secret),
        );

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
        let writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.save_snapshot_locked(path.as_ref(), &writer.working)
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

        let write_and_sync = (|| -> Result<(), String> {
            file.write_all(&line).map_err(|e| e.to_string())?;
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
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
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
        snap.migrate_legacy_service_state();
        snap.reconcile_id_counters();
        snap.backfill_fw_system_occupations();
        for (&sid, st) in &mut snap.system_states {
            st.solar_system_id = sid;
        }
        writer.fp = sqlite_store::replace_sqlite_from_snapshot(&mut writer.conn, &snap)?;
        let _ = writer.conn.execute(
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_block', ?1)",
            rusqlite::params![snap.next_entity_id.max(200_000_000)],
        );
        self.entity_ids
            .reset_from_load(snap.next_entity_id, snap.next_entity_id);
        writer.seq = snap.wal_seq;
        writer.working = snap;
        *self.published.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(writer.working.clone());
        self.bump_state_epoch();
        self.bump_combat_state_epoch();
        Ok(())
    }

    #[track_caller]
    pub(crate) fn read_state(&self) -> std::sync::Arc<WorldStateSnapshot> {
        if !self.is_write_locked_by_current_thread() {
            crate::lock_rank::assert_locks_below(RANK_WORLD_INNER);
        }
        std::sync::Arc::clone(&self.published.read().unwrap_or_else(|e| e.into_inner()))
    }

    #[track_caller]
    pub(crate) fn try_read_state(&self) -> Option<std::sync::Arc<WorldStateSnapshot>> {
        if !self.is_write_locked_by_current_thread() {
            crate::lock_rank::assert_locks_below(RANK_WORLD_INNER);
        }
        self.published
            .try_read()
            .ok()
            .map(|g| std::sync::Arc::clone(&g))
    }

    #[track_caller]
    fn run_read_snapshot(&self, f: &mut dyn FnMut(&WorldStateSnapshot)) {
        let guard = self.read_state();
        f(&guard);
    }

    #[track_caller]
    fn run_try_read_snapshot(&self, f: &mut dyn FnMut(&WorldStateSnapshot)) -> bool {
        if let Some(guard) = self.try_read_state() {
            f(&guard);
            true
        } else {
            false
        }
    }

    /// Executes a single-writer Copy-on-Write transaction against the authoritative working
    /// snapshot and durable SQLite database (`RFC-2 §3.2–§3.4`, `#915`).
    #[track_caller]
    pub fn tx<T, E>(&self, label: &'static str, f: impl FnOnce(&mut WorldTx<'_>) -> Result<T, E>) -> Result<T, E>
    where
        E: crate::tx_engine::FromTxError,
    {
        let mut writer_guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let writer: &mut WorldWriter = &mut *writer_guard;
        let _write_depth_guard = DbWriteLockDepthGuard::enter();

        let before = writer.working.clone();
        let pre_alloc_snap_id = writer.working.next_entity_id;
        let alloc_start = self.entity_ids.current().max(pre_alloc_snap_id);
        writer.working.next_entity_id = alloc_start;

        let mut effects = Effects::default();
        let closure_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut wtx = WorldTx {
                snap: &mut writer.working,
                effects: &mut effects,
                entity_ids: &self.entity_ids,
                conn: &writer.conn,
            };
            f(&mut wtx)
        }));

        let val = match closure_res {
            Err(panic_payload) => {
                let msg = panic_payload_message(&*panic_payload);
                PERSIST_TX_PANICS_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::error!("WorldRepository::tx `{label}` panicked (restoring pre-tx snapshot): {msg}");
                if writer.working.next_entity_id > alloc_start {
                    let _ = self.entity_ids.advance_to(&writer.conn, writer.working.next_entity_id);
                }
                writer.working = before;
                drop(effects);
                return Err(E::from_tx_error(TxError::Panicked { label, msg }));
            }
            Ok(Err(err)) => {
                if writer.working.next_entity_id > alloc_start {
                    let _ = self.entity_ids.advance_to(&writer.conn, writer.working.next_entity_id);
                }
                writer.working = before;
                drop(effects);
                return Err(err);
            }
            Ok(Ok(val)) => val,
        };

        if writer.working.next_entity_id > alloc_start {
            if let Err(err) = self.entity_ids.advance_to(&writer.conn, writer.working.next_entity_id) {
                PERSIST_FAILURES_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                writer.working = before;
                drop(effects);
                return Err(E::from_tx_error(TxError::Persist(err)));
            }
        } else {
            writer.working.next_entity_id = pre_alloc_snap_id;
        }

        for (&sid, st) in &mut writer.working.system_states {
            st.solar_system_id = sid;
        }

        let mut changes = ChangeSet::diff(writer.seq.saturating_add(1), &before, &writer.working);
        let did_mutate = !changes.is_empty();

        if did_mutate {
            if let Err(val_err) =
                validate_changeset(&changes, &before, &writer.working, DEFAULT_MAX_ROW_JSON_BYTES)
            {
                writer.working = before;
                drop(effects);
                return Err(E::from_tx_error(val_err));
            }

            let prev_wal_seq = writer.fp.wal_seq;
            if writer.working.wal_seq <= prev_wal_seq {
                writer.working.wal_seq = prev_wal_seq.saturating_add(1);
                changes.wal_seq = writer.working.wal_seq;
                changes.meta_changed = true;
            }

            let fault_plan =
                TxFaultPlan::decode(self.fault_plan.swap(0, std::sync::atomic::Ordering::SeqCst));
            let need_wal_delta = self.snapshot_path.is_some() && self.sqlite_path.is_none();
            let has_cold_or_meta = changes.meta_changed || changes.cold_domains.any();

            // Stage cold fingerprints without cloning O(N) hot CowMap fingerprint HashMaps.
            let mut staged_cold_fp = if has_cold_or_meta {
                let hot_items = std::mem::take(&mut writer.fp.items);
                let hot_chars = std::mem::take(&mut writer.fp.characters);
                let hot_skills = std::mem::take(&mut writer.fp.skills);
                let hot_queues = std::mem::take(&mut writer.fp.skill_queues);
                let hot_orders = std::mem::take(&mut writer.fp.market_orders);
                let cloned = writer.fp.clone();
                writer.fp.items = hot_items;
                writer.fp.characters = hot_chars;
                writer.fp.skills = hot_skills;
                writer.fp.skill_queues = hot_queues;
                writer.fp.market_orders = hot_orders;
                Some(cloned)
            } else {
                None
            };

            let persist_res = (|| -> Result<(crate::tx_engine::HotFpDelta, Option<WorldStateWalDelta>), TxError> {
                let WorldWriter {
                    ref working,
                    ref mut conn,
                    ref fp,
                    ..
                } = *writer;
                let mut dummy_cold_fp = sqlite_store::TableFingerprints::default();
                let cold_fp_ref = staged_cold_fp.as_mut().unwrap_or(&mut dummy_cold_fp);
                let sql_tx = conn
                    .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                    .map_err(|e| TxError::Persist(e.to_string()))?;
                let mut exec = TxSqlExecutor::new(&sql_tx, fault_plan);
                let (mut delta, hot_fp) = apply_changeset_to_sqlite(
                    &mut exec,
                    working,
                    &changes,
                    fp,
                    cold_fp_ref,
                    need_wal_delta,
                )?;
                exec.check_commit_fault()?;
                if need_wal_delta {
                    if let Some(ref p) = self.snapshot_path {
                        let _wal_guard = self.wal_lock.lock().unwrap_or_else(|e| e.into_inner());
                        delta.seq = working.wal_seq;
                        self.append_wal_delta_locked(p, working, &delta)
                            .map_err(TxError::Persist)?;
                    }
                }
                sql_tx.commit().map_err(|e| TxError::Persist(e.to_string()))?;
                Ok((hot_fp, if need_wal_delta { Some(delta) } else { None }))
            })();

            match persist_res {
                Ok((hot_fp, _wal_delta)) => {
                    if let Some(mut next_fp) = staged_cold_fp {
                        next_fp.items = std::mem::take(&mut writer.fp.items);
                        next_fp.characters = std::mem::take(&mut writer.fp.characters);
                        next_fp.skills = std::mem::take(&mut writer.fp.skills);
                        next_fp.skill_queues = std::mem::take(&mut writer.fp.skill_queues);
                        next_fp.market_orders = std::mem::take(&mut writer.fp.market_orders);
                        next_fp.market_orders_digest = writer.fp.market_orders_digest;
                        writer.fp = next_fp;
                    }
                    hot_fp.apply_to(&mut writer.fp);
                    writer.seq = changes.seq;
                    *self.published.write().unwrap_or_else(|e| e.into_inner()) =
                        std::sync::Arc::new(writer.working.clone());
                    self.tx_commit_count
                        .fetch_add(1, std::sync::atomic::Ordering::Release);
                    self.bump_state_epoch();
                    if changes.cold_domains.crimewatch_changed {
                        self.bump_combat_state_epoch();
                    }
                }
                Err(persist_err) => {
                    PERSIST_FAILURES_TOTAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    tracing::error!(
                        "WorldRepository::tx `{label}` SQLite commit aborted (restoring pre-tx snapshot): {persist_err}"
                    );
                    writer.working = before;
                    drop(effects);
                    return Err(E::from_tx_error(persist_err));
                }
            }
        }

        if !effects.is_empty() {
            let listeners: Vec<PostCommitListener> = self
                .post_commit_listeners
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .map(|(_, l)| std::sync::Arc::clone(l))
                .collect();
            for listener in listeners {
                listener(&writer.working, &changes, &effects);
            }
            effects.run_callbacks(&changes);
        }

        Ok(val)
    }

    #[track_caller]
    pub fn with_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&WorldStateSnapshot) -> R,
    {
        let mut f_opt = Some(f);
        let mut out: Option<R> = None;
        self.run_read_snapshot(&mut |snap| {
            if let Some(cb) = f_opt.take() {
                out = Some(cb(snap));
            }
        });
        out.expect("with_snapshot closure must run")
    }

    #[track_caller]
    pub fn try_with_snapshot<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&WorldStateSnapshot) -> R,
    {
        let mut f_opt = Some(f);
        let mut out: Option<R> = None;
        if self.run_try_read_snapshot(&mut |snap| {
            if let Some(cb) = f_opt.take() {
                out = Some(cb(snap));
            }
        }) {
            out
        } else {
            None
        }
    }

    /// Thin wrapper over [`Self::tx`] for infallible callers.
    /// Panics if the closure itself panics or if durable validation/SQLite commit fails.
    #[track_caller]
    pub fn with_mut_snapshot<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WorldStateSnapshot) -> R,
    {
        match self.tx("with_mut_snapshot", |wtx| Ok::<R, TxError>(f(wtx.snap))) {
            Ok(val) => val,
            Err(err) => panic!("{err}"),
        }
    }

    /// Mutates the in-memory `WorldStateSnapshot` directly without running a SQLite/WAL commit,
    /// allowing unit tests to inject corrupted in-memory state (such as `NaN` or `Infinity` floats
    /// that `FiniteFloatChecker` rejects at SQLite commit time) to test defensive RPC guards.
    #[doc(hidden)]
    pub fn corrupt_in_memory_snapshot_for_test<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut WorldStateSnapshot) -> R,
    {
        let mut writer = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        let out = f(&mut writer.working);
        writer.fp = sqlite_store::compute_fingerprints(&writer.working);
        *self.published.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(writer.working.clone());
        out
    }

    /// Returns `true` if the current thread currently holds the `WorldRepository` write lock
    /// (inside `tx` / `with_mut_snapshot` / `try_with_mut_snapshot`). Used to prevent re-entrant
    /// same-thread deadlocks from synchronous callbacks such as `SystemHandle::try_remove_ball`.
    pub fn is_write_locked_by_current_thread(&self) -> bool {
        DB_WRITE_LOCK_DEPTH.with(|d| d.get() > 0)
    }

    /// Thin wrapper over [`Self::tx`] preserving [`FromWalError`] compatibility for existing
    /// domain callers (`RFC-2 §3.2`).
    #[track_caller]
    pub fn try_with_mut_snapshot<F, T, E>(&self, f: F) -> Result<T, E>
    where
        F: FnOnce(&mut WorldStateSnapshot) -> Result<T, E>,
        E: FromWalError,
    {
        enum AdapterErr<E> {
            User(E),
            Tx(TxError),
        }
        impl<E> crate::tx_engine::FromTxError for AdapterErr<E> {
            fn from_tx_error(err: TxError) -> Self {
                Self::Tx(err)
            }
        }
        match self.tx("try_with_mut_snapshot", |wtx| {
            f(wtx.snap).map_err(AdapterErr::User)
        }) {
            Ok(val) => Ok(val),
            Err(AdapterErr::User(e)) => Err(e),
            Err(AdapterErr::Tx(tx_err)) => Err(E::from_wal_error(tx_err.to_string())),
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
            snap.asteroids.insert(record.asteroid_id, record);
        });
    }
}

#[cfg(test)]
mod r4_persistence_tests {
    use super::*;

    #[test]
    fn test_r4_198_wal_delta_cold_equal_detects_blobs_and_corp_maps() {
        let old = WorldStateSnapshot::default();

        let mut new_svc = old.clone();
        new_svc.service_state_blobs.insert("cache_setting:test:k".into(), "10000".into());
        let delta_svc = WorldStateWalDelta::from_diff(&old, &new_svc);
        assert!(delta_svc.full_snapshot.is_some(), "service_state_blobs change must trigger full_snapshot");

        let mut new_domain = old.clone();
        new_domain.fleet_and_social_ops.contract_deposits.insert(1, 10_000);
        let delta_domain = WorldStateWalDelta::from_diff(&old, &new_domain);
        assert!(delta_domain.full_snapshot.is_some(), "typed domain field change must trigger full_snapshot");

        let mut new_quasar = old.clone();
        new_quasar.quasar_state_blobs.insert("entitlement_ledger".into(), "{}".into());
        let delta_quasar = WorldStateWalDelta::from_diff(&old, &new_quasar);
        assert!(delta_quasar.full_snapshot.is_some(), "quasar_state_blobs change must trigger full_snapshot");

        let mut new_limits = old.clone();
        new_limits.corp_member_limits.insert(98_000_001, 50);
        let delta_limits = WorldStateWalDelta::from_diff(&old, &new_limits);
        assert!(delta_limits.full_snapshot.is_some(), "corp_member_limits change must trigger full_snapshot");
    }

    #[test]
    fn test_system_states_rollback_preserves_solar_system_id() {
        let tmp_path = std::env::temp_dir().join(format!("evomu_db01_rollback_{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&tmp_path);
        let repo = WorldRepository::open_sqlite(&tmp_path).expect("open_sqlite");
        let sys_id = 30_045_329u32;
        repo.with_mut_snapshot(|snap| {
            // Even when inserted via `or_default()` (`solar_system_id: 0`), commit hydrates `solar_system_id`.
            let st = snap.system_states.entry(sys_id).or_default();
            st.insurgency_corruption_stage = 3;
        });
        assert_eq!(
            repo.with_snapshot(|s| s.system_states[&sys_id].solar_system_id),
            sys_id
        );

        let err_res: Result<(), String> = repo.try_with_mut_snapshot(|snap| {
            let st = snap.system_states.get_mut(&sys_id).unwrap();
            st.solar_system_id = 0;
            st.insurgency_corruption_stage = 5;
            Err("simulated transaction failure".into())
        });
        assert!(err_res.is_err());

        repo.with_snapshot(|snap| {
            let st = &snap.system_states[&sys_id];
            assert_eq!(
                st.solar_system_id, sys_id,
                "aborted transaction must preserve pre-transaction solar_system_id"
            );
            assert_eq!(st.insurgency_corruption_stage, 3);
        });
        drop(repo);
        let _ = std::fs::remove_file(&tmp_path);
    }

    #[test]
    fn test_prune_expired_auth_runtime_respects_lock_ranks() {
        let repo = WorldRepository::new_empty();
        assert_eq!(repo.prune_expired_auth_runtime(1_000), 0);
        assert!(crate::lock_rank::held_lock_ranks().is_empty());

        let _leaf = repo.auth_runtime.read().unwrap();
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            repo.with_snapshot(|_| ());
        }));
        assert!(
            res.is_err(),
            "world transaction entry while holding RANK_LEAF must panic"
        );
    }
}
#[cfg(test)] #[path = "persistence_unit_tests.rs"] mod persistence_unit_tests;
#[cfg(test)] #[path = "persistence_edges_tests.rs"] mod persistence_edges_tests;


