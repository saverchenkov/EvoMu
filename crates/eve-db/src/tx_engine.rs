//! Copy-on-Write transaction engine, diffing (`ChangeSet`), post-commit `Effects`,
//! external block-persisted `EntityIdAllocator`, and SQLite fault-plan injection (`RFC-2 §3.2–§3.4`).

use crate::cow::CowDiffItem;
use crate::sqlite_store::fingerprints::{FiniteFloatChecker, TableFingerprints};
use crate::types::{
    CharacterRecord, DbError, InventoryItemRecord, MarketOrderRecord, SkillQueueEntry,
    TrainedSkillRecord, TypeId, WorldStateSnapshot,
};
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// Block size persisted to `world_meta.next_entity_block` for crash/abort-safe ID allocation (`P-9`).
pub const ENTITY_ID_BLOCK_SIZE: i64 = 1024;

/// Default maximum serialized JSON byte size permitted for a single row (`TxError::RowLimit`).
pub const DEFAULT_MAX_ROW_JSON_BYTES: usize = 4 * 1024 * 1024;

/// Transaction error returned by [`crate::WorldRepository::tx`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TxError {
    #[error("database persistence failed: {0}")]
    Persist(String),
    #[error("transaction closure `{label}` panicked: {msg}")]
    Panicked { label: &'static str, msg: String },
    #[error("non-finite float in table `{table}` key `{key}`")]
    NonFinite { table: &'static str, key: String },
    #[error("row payload limit exceeded in table `{table}` key `{key}` ({bytes} bytes)")]
    RowLimit {
        table: &'static str,
        key: String,
        bytes: usize,
    },
}

/// Converts a [`TxError`] into a caller error type without adding blanket `From<TxError>` impls
/// that would introduce `?` type-inference ambiguity on legacy `FromWalError` call sites.
pub trait FromTxError {
    fn from_tx_error(err: TxError) -> Self;
}

impl FromTxError for TxError {
    #[inline]
    fn from_tx_error(err: TxError) -> Self {
        err
    }
}

impl FromTxError for DbError {
    fn from_tx_error(err: TxError) -> Self {
        match &err {
            TxError::NonFinite { .. } | TxError::RowLimit { .. } => {
                DbError::ValidationError(err.to_string())
            }
            TxError::Persist(_) | TxError::Panicked { .. } => {
                DbError::PersistenceFailure(err.to_string())
            }
        }
    }
}

impl FromTxError for eve_proto::BridgeRpcError {
    fn from_tx_error(err: TxError) -> Self {
        let code = match &err {
            TxError::NonFinite { .. } | TxError::RowLimit { .. } => "ValidationError",
            TxError::Panicked { .. } => "InternalError",
            TxError::Persist(_) => "PersistenceFailure",
        };
        eve_proto::BridgeRpcError {
            code: code.into(),
            message: err.to_string(),
        }
    }
}

impl FromTxError for String {
    fn from_tx_error(err: TxError) -> Self {
        err.to_string()
    }
}

impl FromTxError for (u32, String) {
    fn from_tx_error(err: TxError) -> Self {
        (500, err.to_string())
    }
}

impl FromTxError for () {
    fn from_tx_error(_: TxError) -> Self {}
}

/// Single modified, inserted, or removed row shared with snapshots via `Arc<V>` (`RFC-2 §3.4`).
#[derive(Debug, Clone, PartialEq)]
pub struct RowChange<K, V> {
    pub key: K,
    pub before: Option<Arc<V>>,
    pub after: Option<Arc<V>>,
}

/// Flags indicating which cold (non-`CowMap`) snapshot domains changed during a transaction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ColdDomainDiff {
    pub part1_cold: bool,
    pub part2_activity: bool,
    pub part3_social_corp: bool,
    pub part4_world_tail: bool,
    pub extension_tables: bool,
    pub ledger: bool,
    pub d6_notes_lp_seq: bool,
    pub d6_social: bool,
    pub d6_corp_crimewatch: bool,
    pub d6_batch4: bool,
    pub service_domains: bool,
    pub crimewatch_changed: bool,
}

impl ColdDomainDiff {
    /// Returns `true` if any cold domain changed.
    #[inline]
    pub fn any(&self) -> bool {
        self.part1_cold
            || self.part2_activity
            || self.part3_social_corp
            || self.part4_world_tail
            || self.extension_tables
            || self.ledger
            || self.d6_notes_lp_seq
            || self.d6_social
            || self.d6_corp_crimewatch
            || self.d6_batch4
            || self.service_domains
    }
}

/// Structural diff between `before` and `after` snapshots (`RFC-2 §3.4`).
///
/// Hot `CowMap` tables (`items`, `characters`, `skills`, `skill_queues`, `market_orders`) are
/// diffed in `O(k log N)` without scanning unchanged rows.
#[derive(Debug, Clone, Default)]
pub struct ChangeSet {
    pub seq: u64,
    pub meta_changed: bool,
    pub next_entity_id: i64,
    pub wal_seq: u64,
    pub markets_seeded: bool,
    pub items: Vec<RowChange<i64, InventoryItemRecord>>,
    pub characters: Vec<RowChange<i64, CharacterRecord>>,
    pub skills: Vec<RowChange<i64, HashMap<TypeId, TrainedSkillRecord>>>,
    pub skill_queues: Vec<RowChange<i64, Vec<SkillQueueEntry>>>,
    pub market_orders: Vec<RowChange<i64, MarketOrderRecord>>,
    pub cold_changed: bool,
    pub cold_domains: ColdDomainDiff,
}

impl ChangeSet {
    /// Returns `true` when zero metadata, hot-table, or cold-domain changes occurred.
    #[inline]
    pub fn is_empty(&self) -> bool {
        !self.meta_changed
            && self.items.is_empty()
            && self.characters.is_empty()
            && self.skills.is_empty()
            && self.skill_queues.is_empty()
            && self.market_orders.is_empty()
            && !self.cold_changed
    }

    /// Computes a `ChangeSet` between `before` and `after` snapshots (`O(k log N)` for `CowMap`s).
    pub fn diff(seq: u64, before: &WorldStateSnapshot, after: &WorldStateSnapshot) -> Self {
        let meta_changed = before.next_entity_id != after.next_entity_id
            || before.wal_seq != after.wal_seq
            || before.markets_seeded != after.markets_seeded;

        let items = diff_cow_map(&before.items, &after.items);
        let characters = diff_cow_map(&before.characters, &after.characters);
        let skills = diff_cow_map(&before.skills, &after.skills);
        let skill_queues = diff_cow_map(&before.skill_queues, &after.skill_queues);
        let market_orders = diff_cow_map(&before.market_orders, &after.market_orders);

        let cold_domains = compute_cold_domain_diff(before, after);
        let cold_changed = cold_domains.any()
            || before.markets_seeded != after.markets_seeded
            || items.iter().any(|c| c.after.is_none())
            || characters.iter().any(|c| c.after.is_none())
            || skills.iter().any(|c| c.after.is_none())
            || skill_queues.iter().any(|c| c.after.is_none())
            || market_orders.iter().any(|c| c.after.is_none());

        Self {
            seq,
            meta_changed,
            next_entity_id: after.next_entity_id,
            wal_seq: after.wal_seq,
            markets_seeded: after.markets_seeded,
            items,
            characters,
            skills,
            skill_queues,
            market_orders,
            cold_changed,
            cold_domains,
        }
    }
}

fn diff_cow_map<K: Ord + Clone, V: Clone + PartialEq>(
    before: &crate::cow::CowMap<K, V>,
    after: &crate::cow::CowMap<K, V>,
) -> Vec<RowChange<K, V>> {
    if before.ptr_eq(after) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for item in before.diff(after) {
        match item {
            CowDiffItem::Add(k, after_arc) => out.push(RowChange {
                key: k.clone(),
                before: None,
                after: Some(Arc::clone(after_arc)),
            }),
            CowDiffItem::Update {
                old: (_, before_arc),
                new: (k, after_arc),
            } => out.push(RowChange {
                key: k.clone(),
                before: Some(Arc::clone(before_arc)),
                after: Some(Arc::clone(after_arc)),
            }),
            CowDiffItem::Remove(k, before_arc) => out.push(RowChange {
                key: k.clone(),
                before: Some(Arc::clone(before_arc)),
                after: None,
            }),
        }
    }
    out
}

fn compute_cold_domain_diff(before: &WorldStateSnapshot, after: &WorldStateSnapshot) -> ColdDomainDiff {
    let part1_cold = before.accounts != after.accounts
        || before.jump_clones != after.jump_clones
        || before.active_implants != after.active_implants
        || before.saved_fittings != after.saved_fittings
        || before.wallet_journal != after.wallet_journal;

    let part2_activity = before.contracts != after.contracts
        || before.industry_jobs != after.industry_jobs
        || before.planetary_colonies != after.planetary_colonies
        || before.active_missions != after.active_missions
        || before.mission_tally != after.mission_tally
        || before.standing_transactions != after.standing_transactions
        || before.npe_progress != after.npe_progress
        || before.bookmarks != after.bookmarks
        || before.mails != after.mails;

    let part3_social_corp = before.fleets != after.fleets
        || before.corporations != after.corporations
        || before.alliances != after.alliances
        || before.wars != after.wars
        || before.corp_projects != after.corp_projects
        || before.killmails != after.killmails
        || before.kill_rights != after.kill_rights;

    let part4_world_tail = before.structures != after.structures
        || before.system_states != after.system_states
        || before.committed_idempotency_keys != after.committed_idempotency_keys
        || before.licensed_skins != after.licensed_skins
        || before.applied_ship_skins != after.applied_ship_skins
        || before.asteroids != after.asteroids;

    let extension_tables = before.character_last_jump_unix != after.character_last_jump_unix
        || before.character_cspa_charges != after.character_cspa_charges
        || before.mail_timestamps != after.mail_timestamps
        || before.character_login_stats != after.character_login_stats
        || before.item_custom_names != after.item_custom_names
        || before.item_positions != after.item_positions
        || before.structure_vulnerability_windows != after.structure_vulnerability_windows
        || before.skill_history != after.skill_history
        || before.character_doll_info != after.character_doll_info;

    let ledger = before.wallet_journal_meta != after.wallet_journal_meta
        || before.market_transactions != after.market_transactions
        || before.account_plex_vault != after.account_plex_vault;

    let d6_notes_lp_seq = before.owner_notes != after.owner_notes
        || before.corp_lp_balances != after.corp_lp_balances
        || before.char_lp_logs != after.char_lp_logs
        || before.next_alliance_id != after.next_alliance_id
        || before.next_corporation_id != after.next_corporation_id
        || before.next_notification_id != after.next_notification_id
        || before.next_report_id != after.next_report_id;

    let d6_social = before.read_mails != after.read_mails
        || before.deleted_mails != after.deleted_mails
        || before.trashed_mails != after.trashed_mails
        || before.mail_labels != after.mail_labels
        || before.mail_assigned_labels != after.mail_assigned_labels
        || before.mailing_lists != after.mailing_lists
        || before.calendar_events != after.calendar_events
        || before.bookmark_subfolders != after.bookmark_subfolders
        || before.bookmark_subfolder_parents != after.bookmark_subfolder_parents
        || before.bookmark_expiries != after.bookmark_expiries
        || before.bookmark_folder_acls != after.bookmark_folder_acls
        || before.access_groups != after.access_groups
        || before.bookmark_folder_known_by != after.bookmark_folder_known_by
        || before.chat_channels != after.chat_channels
        || before.chat_channel_acl != after.chat_channel_acl
        || before.char_contacts != after.char_contacts
        || before.char_contact_notifications != after.char_contact_notifications
        || before.char_contact_labels != after.char_contact_labels
        || before.corp_contacts != after.corp_contacts
        || before.corp_contact_labels != after.corp_contact_labels
        || before.alliance_contacts != after.alliance_contacts
        || before.alliance_contact_labels != after.alliance_contact_labels
        || before.char_notes != after.char_notes;

    let crimewatch_changed = before.crimewatch_states != after.crimewatch_states
        || before.crimewatch_aggressions != after.crimewatch_aggressions
        || before.combat_crimewatch_state != after.combat_crimewatch_state;

    let d6_corp_crimewatch = crimewatch_changed
        || before.corp_governance != after.corp_governance
        || before.employment_history != after.employment_history
        || before.alliance_war_state != after.alliance_war_state
        || before.war_state != after.war_state
        || before.fw_runtime != after.fw_runtime
        || before.sov_runtime != after.sov_runtime
        || before.insurgency_runtime != after.insurgency_runtime
        || before.zarzakh_gate_locks != after.zarzakh_gate_locks
        || before.structure_docking_acl_denied != after.structure_docking_acl_denied
        || before.sp6_lifecycle != after.sp6_lifecycle;

    let d6_batch4 = before.moon_extractions != after.moon_extractions
        || before.auto_moon_miners != after.auto_moon_miners
        || before.observer_mining_ledger != after.observer_mining_ledger
        || before.char_research_agents != after.char_research_agents
        || before.planetary_colony_layouts != after.planetary_colony_layouts
        || before.corp_customs_offices != after.corp_customs_offices
        || before.orbital_provision_types != after.orbital_provision_types
        || before.wallet_journal_timestamps != after.wallet_journal_timestamps
        || before.char_wallet_transactions != after.char_wallet_transactions
        || before.corp_wallet_journals != after.corp_wallet_journals
        || before.corp_wallet_transactions != after.corp_wallet_transactions
        || before.market_order_meta != after.market_order_meta
        || before.market_history != after.market_history
        || before.cosmic_signatures != after.cosmic_signatures
        || before.cosmic_anomalies != after.cosmic_anomalies
        || before.incursions != after.incursions
        || before.last_mission_decline_times != after.last_mission_decline_times
        || before.wh_polarizations != after.wh_polarizations
        || before.module_online_states != after.module_online_states
        || before.module_active_states != after.module_active_states
        || before.module_active_charges != after.module_active_charges
        || before.weapon_banks != after.weapon_banks
        || before.paused_skill_queues != after.paused_skill_queues
        || before.starbase_settings != after.starbase_settings
        || before.corp_container_logs != after.corp_container_logs
        || before.killmail_details != after.killmail_details
        || before.station_office_rental_costs != after.station_office_rental_costs
        || before.structure_esi_timer_meta != after.structure_esi_timer_meta
        || before.corp_project_contributors != after.corp_project_contributors
        || before.loyalty_store_offers != after.loyalty_store_offers
        || before.system_jump_events != after.system_jump_events
        || before.system_kill_events != after.system_kill_events
        || before.quasar_state_blobs != after.quasar_state_blobs
        || before.service_state_blobs != after.service_state_blobs
        || before.insurance_contracts != after.insurance_contracts
        || before.corp_vote_cases != after.corp_vote_cases
        || before.corp_vote_ballots != after.corp_vote_ballots
        || before.corp_alliance_history != after.corp_alliance_history
        || before.corp_medals != after.corp_medals
        || before.corp_issued_medals != after.corp_issued_medals
        || before.corp_member_limits != after.corp_member_limits
        || before.corp_titles != after.corp_titles
        || before.corp_member_titles != after.corp_member_titles
        || before.corp_hangar_divisions != after.corp_hangar_divisions
        || before.war_lifecycle_meta != after.war_lifecycle_meta
        || before.alliance_lifecycle_meta != after.alliance_lifecycle_meta
        || before.fw_character_stats != after.fw_character_stats
        || before.fw_corp_enlisted_on != after.fw_corp_enlisted_on
        || before.fw_system_occupations != after.fw_system_occupations
        || before.fw_wars != after.fw_wars
        || before.sov_prospecting_levels != after.sov_prospecting_levels
        || before.sov_campaign_scores != after.sov_campaign_scores
        || before.mining_ledger != after.mining_ledger;

    let service_domains = before.pve_special_sites != after.pve_special_sites
        || before.crab_beacons != after.crab_beacons
        || before.abyssal_state != after.abyssal_state
        || before.dungeon_runtime != after.dungeon_runtime
        || before.dungeon_editor != after.dungeon_editor
        || before.fob_observatory_state != after.fob_observatory_state
        || before.mining_and_ess_state != after.mining_and_ess_state
        || before.pvp_filament_state != after.pvp_filament_state
        || before.tourney_state != after.tourney_state
        || before.deployable_state != after.deployable_state
        || before.superweapon_runtime != after.superweapon_runtime
        || before.insurgency_complex_ops != after.insurgency_complex_ops
        || before.node_graph_state != after.node_graph_state
        || before.seasons_and_campaigns != after.seasons_and_campaigns
        || before.quasar_commerce != after.quasar_commerce
        || before.fleet_and_social_ops != after.fleet_and_social_ops
        || before.cluster_and_runtime != after.cluster_and_runtime
        || before.entity_kv != after.entity_kv;

    ColdDomainDiff {
        part1_cold,
        part2_activity,
        part3_social_corp,
        part4_world_tail,
        extension_tables,
        ledger,
        d6_notes_lp_seq,
        d6_social,
        d6_corp_crimewatch,
        d6_batch4,
        service_domains,
        crimewatch_changed,
    }
}

fn validate_row<T: Serialize + ?Sized>(
    table: &'static str,
    key: String,
    val: &T,
    max_row_bytes: usize,
) -> Result<(), TxError> {
    if val.serialize(FiniteFloatChecker).is_err() {
        return Err(TxError::NonFinite { table, key });
    }
    let bytes = serde_json::to_vec(val)
        .map_err(|e| TxError::Persist(e.to_string()))?
        .len();
    if bytes > max_row_bytes {
        return Err(TxError::RowLimit { table, key, bytes });
    }
    Ok(())
}

/// Validates all modified rows in `changes` (and changed cold domains) for finite floats and
/// payload size limits before opening SQLite `BEGIN IMMEDIATE`.
pub fn validate_changeset(
    changes: &ChangeSet,
    before: &WorldStateSnapshot,
    after: &WorldStateSnapshot,
    max_row_bytes: usize,
) -> Result<(), TxError> {
    for c in &changes.items {
        if let Some(ref v) = c.after {
            validate_row("items", c.key.to_string(), &**v, max_row_bytes)?;
        }
    }
    for c in &changes.characters {
        if let Some(ref v) = c.after {
            validate_row("characters", c.key.to_string(), &**v, max_row_bytes)?;
        }
    }
    for c in &changes.skills {
        if let Some(ref map) = c.after {
            for (tid, sk) in map.iter() {
                validate_row("skills", format!("{}:{}", c.key, tid), sk, max_row_bytes)?;
            }
        }
    }
    for c in &changes.skill_queues {
        if let Some(ref q) = c.after {
            validate_row("skill_queues", c.key.to_string(), &**q, max_row_bytes)?;
        }
    }
    for c in &changes.market_orders {
        if let Some(ref o) = c.after {
            validate_row("market_orders", c.key.to_string(), &**o, max_row_bytes)?;
        }
    }

    let cd = &changes.cold_domains;
    if cd.part1_cold {
        for (k, v) in &after.saved_fittings {
            if before.saved_fittings.get(k) != Some(v) {
                validate_row("saved_fittings", k.to_string(), v, max_row_bytes)?;
            }
        }
    }
    if cd.part2_activity {
        for (k, v) in &after.contracts {
            if before.contracts.get(k) != Some(v) {
                validate_row("contracts", k.to_string(), v, max_row_bytes)?;
            }
        }
        for (k, v) in &after.industry_jobs {
            if before.industry_jobs.get(k) != Some(v) {
                validate_row("industry_jobs", k.to_string(), v, max_row_bytes)?;
            }
        }
        for (k, v) in &after.bookmarks {
            if before.bookmarks.get(k) != Some(v) {
                validate_row("bookmarks", k.to_string(), v, max_row_bytes)?;
            }
        }
    }
    if cd.part4_world_tail {
        for (k, v) in &after.structures {
            if before.structures.get(k) != Some(v) {
                validate_row("structures", k.to_string(), v, max_row_bytes)?;
            }
        }
        for (k, v) in &after.system_states {
            if before.system_states.get(k) != Some(v) {
                validate_row("system_states", k.to_string(), v, max_row_bytes)?;
            }
        }
        for (k, v) in &after.asteroids {
            if before.asteroids.get(k) != Some(v) {
                validate_row("asteroids", k.to_string(), v, max_row_bytes)?;
            }
        }
    }
    if cd.extension_tables && before.item_positions != after.item_positions {
        for (k, v) in &after.item_positions {
            if before.item_positions.get(k) != Some(v) {
                validate_row("item_positions", k.to_string(), v, max_row_bytes)?;
            }
        }
    }
    if cd.d6_corp_crimewatch {
        if before.crimewatch_states != after.crimewatch_states {
            validate_row("d6_corp_crimewatch", "crimewatch_states".into(), &after.crimewatch_states, max_row_bytes)?;
        }
        if before.sp6_lifecycle != after.sp6_lifecycle {
            validate_row("d6_corp_crimewatch", "sp6_lifecycle".into(), &after.sp6_lifecycle, max_row_bytes)?;
        }
    }
    Ok(())
}

/// Notification target carried by [`TxNotificationEffect`] inside [`Effects`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxNotificationTarget {
    Character(i64),
    Corporation(i64),
    Alliance(i64),
    SolarSystem(u32),
}

/// Unencoded notification queued during a transaction and dispatched inside the critical section
/// immediately after SQLite `COMMIT` and snapshot publication (`RFC-2 §3.2`, P-4).
#[derive(Debug, Clone, PartialEq)]
pub struct TxNotificationEffect {
    pub target: TxNotificationTarget,
    pub event: String,
    pub id_type: String,
    pub args: Vec<eve_proto::PyRep>,
}

/// Post-commit effects accumulated inside [`WorldTx`] and dropped automatically on abort/panic.
#[derive(Default)]
pub struct Effects {
    pub notifications: Vec<TxNotificationEffect>,
    callbacks: Vec<Box<dyn FnOnce(&ChangeSet) + Send>>,
}

impl Effects {
    /// Queues a character notification to be delivered only if this transaction commits.
    pub fn notify_character(
        &mut self,
        character_id: i64,
        event: impl Into<String>,
        id_type: impl Into<String>,
        args: Vec<eve_proto::PyRep>,
    ) {
        self.notifications.push(TxNotificationEffect {
            target: TxNotificationTarget::Character(character_id),
            event: event.into(),
            id_type: id_type.into(),
            args,
        });
    }

    /// Queues a corporation-wide notification to be delivered only if this transaction commits.
    pub fn notify_corp(
        &mut self,
        corporation_id: i64,
        event: impl Into<String>,
        id_type: impl Into<String>,
        args: Vec<eve_proto::PyRep>,
    ) {
        self.notifications.push(TxNotificationEffect {
            target: TxNotificationTarget::Corporation(corporation_id),
            event: event.into(),
            id_type: id_type.into(),
            args,
        });
    }

    /// Queues a targeted notification to be delivered only if this transaction commits.
    pub fn notify_target(
        &mut self,
        target: TxNotificationTarget,
        event: impl Into<String>,
        id_type: impl Into<String>,
        args: Vec<eve_proto::PyRep>,
    ) {
        self.notifications.push(TxNotificationEffect {
            target,
            event: event.into(),
            id_type: id_type.into(),
            args,
        });
    }

    /// Registers an in-process post-commit callback run inside the critical section after `COMMIT`.
    pub fn on_commit(&mut self, f: impl FnOnce(&ChangeSet) + Send + 'static) {
        self.callbacks.push(Box::new(f));
    }

    /// Returns `true` when no notifications or callbacks are queued.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.notifications.is_empty() && self.callbacks.is_empty()
    }

    /// Executes all queued callbacks in commit order.
    pub(crate) fn run_callbacks(self, changes: &ChangeSet) {
        for cb in self.callbacks {
            cb(changes);
        }
    }
}

/// Listener notified inside the writer critical section after SQLite `COMMIT` and snapshot publish.
pub type PostCommitListener = Arc<dyn Fn(&WorldStateSnapshot, &ChangeSet, &Effects) + Send + Sync>;

/// External entity ID allocator with a block-persisted high-water mark (`P-9`, `RFC-2 §3.2`).
///
/// Resides outside [`WorldStateSnapshot`] so aborted transactions burn IDs and never reissue them.
#[derive(Debug)]
pub struct EntityIdAllocator {
    next: AtomicI64,
    persisted_high_water: AtomicI64,
}

impl EntityIdAllocator {
    /// Creates an allocator starting at `initial_next`, with `persisted_high_water` already
    /// durably recorded in `world_meta.next_entity_block`.
    pub fn new(initial_next: i64, persisted_high_water: i64) -> Self {
        let floor = initial_next.max(200_000_000);
        Self {
            next: AtomicI64::new(floor),
            persisted_high_water: AtomicI64::new(persisted_high_water.max(200_000_000)),
        }
    }

    /// Returns the most recently allocated (or initialized) ID counter without incrementing it.
    #[inline]
    pub fn current(&self) -> i64 {
        self.next.load(Ordering::Acquire)
    }

    /// Returns the persisted high-water block bound currently stored in `world_meta`.
    #[inline]
    pub fn persisted_high_water(&self) -> i64 {
        self.persisted_high_water.load(Ordering::Acquire)
    }

    /// Resets in-memory allocator bounds (used when loading a full snapshot file into memory).
    pub(crate) fn reset_from_load(&self, next: i64, persisted_high_water: i64) {
        self.next.store(next.max(200_000_000), Ordering::Release);
        self.persisted_high_water
            .store(persisted_high_water.max(200_000_000), Ordering::Release);
    }

    /// Allocates a strictly monotonic entity ID, immediately persisting the next block of
    /// [`ENTITY_ID_BLOCK_SIZE`] into `world_meta.next_entity_block` in autocommit mode if crossed.
    pub fn alloc_with_conn(&self, conn: &Connection) -> Result<i64, String> {
        let id = self.next.fetch_add(1, Ordering::AcqRel) + 1;
        self.ensure_block_persisted(conn, id)?;
        Ok(id)
    }

    /// Advances the allocator to at least `floor` (preserving monotonic IDs allocated via legacy
    /// snapshot accessors even across aborts) and persists any crossed block.
    pub(crate) fn advance_to(&self, conn: &Connection, floor: i64) -> Result<(), String> {
        let mut cur = self.next.load(Ordering::Acquire);
        while cur < floor {
            match self.next.compare_exchange_weak(
                cur,
                floor,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => cur = observed,
            }
        }
        self.ensure_block_persisted(conn, floor)
    }

    fn ensure_block_persisted(&self, conn: &Connection, id: i64) -> Result<(), String> {
        let hwm = self.persisted_high_water.load(Ordering::Acquire);
        if id <= hwm {
            return Ok(());
        }
        let blocks = (id - 1).div_euclid(ENTITY_ID_BLOCK_SIZE) + 1;
        let new_block = blocks.saturating_mul(ENTITY_ID_BLOCK_SIZE).max(id);
        // Must commit outside any open data transaction so a data rollback never reverts the block.
        conn.execute(
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_block', ?1)",
            params![new_block],
        )
        .map_err(|e| e.to_string())?;
        let mut prev = self.persisted_high_water.load(Ordering::Acquire);
        while prev < new_block {
            match self.persisted_high_water.compare_exchange_weak(
                prev,
                new_block,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => prev = observed,
            }
        }
        Ok(())
    }
}

/// Active transaction context passed to closures in [`crate::WorldRepository::tx`].
pub struct WorldTx<'a> {
    pub snap: &'a mut WorldStateSnapshot,
    pub effects: &'a mut Effects,
    pub(crate) entity_ids: &'a EntityIdAllocator,
    pub(crate) conn: &'a Connection,
}

impl<'a> WorldTx<'a> {
    /// Allocates a strictly monotonic entity ID from the external [`EntityIdAllocator`].
    ///
    /// Aborting or panicking after calling this method still burns the allocated ID and keeps the
    /// persisted `world_meta.next_entity_block` high-water mark so IDs are never reissued.
    pub fn alloc_entity_id(&mut self) -> Result<i64, TxError> {
        self.entity_ids
            .alloc_with_conn(self.conn)
            .map_err(TxError::Persist)
    }
}

/// Fault injection plan for verifying transactional atomicity (`#915`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxFaultPlan {
    /// Normal execution with no injected faults.
    #[default]
    None,
    /// Fail immediately before executing the `N`th SQLite statement (1-indexed) inside `BEGIN IMMEDIATE`.
    FailNthStatement(usize),
    /// Fail right at SQLite `COMMIT` so all prior statements roll back.
    FailAtCommit,
}

impl TxFaultPlan {
    pub(crate) fn encode(self) -> u64 {
        match self {
            Self::None => 0,
            Self::FailNthStatement(n) => n.max(1).min((u64::MAX - 1) as usize) as u64,
            Self::FailAtCommit => u64::MAX,
        }
    }

    pub(crate) fn decode(raw: u64) -> Self {
        match raw {
            0 => Self::None,
            u64::MAX => Self::FailAtCommit,
            n => Self::FailNthStatement(n as usize),
        }
    }
}

/// Executes SQLite statements during a transaction while optionally injecting a [`TxFaultPlan`].
pub(crate) struct TxSqlExecutor<'a, 'conn> {
    pub(crate) tx: &'a Transaction<'conn>,
    fault_plan: TxFaultPlan,
    stmt_index: usize,
}

impl<'a, 'conn> TxSqlExecutor<'a, 'conn> {
    pub(crate) fn new(tx: &'a Transaction<'conn>, fault_plan: TxFaultPlan) -> Self {
        Self {
            tx,
            fault_plan,
            stmt_index: 0,
        }
    }

    pub(crate) fn step_fault_check(&mut self, label: &str) -> Result<(), TxError> {
        self.stmt_index = self.stmt_index.saturating_add(1);
        if let TxFaultPlan::FailNthStatement(target) = self.fault_plan {
            if self.stmt_index == target {
                return Err(TxError::Persist(format!(
                    "injected fault at SQLite statement #{target} ({label})"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn execute<P: rusqlite::Params>(
        &mut self,
        label: &str,
        sql: &str,
        params: P,
    ) -> Result<usize, TxError> {
        self.step_fault_check(label)?;
        self.tx
            .execute(sql, params)
            .map_err(|e| TxError::Persist(e.to_string()))
    }

    pub(crate) fn check_commit_fault(&self) -> Result<(), TxError> {
        if matches!(self.fault_plan, TxFaultPlan::FailAtCommit) {
            return Err(TxError::Persist(
                "injected fault at SQLite COMMIT".to_string(),
            ));
        }
        Ok(())
    }
}

/// Deferred `O(k)` hot-table fingerprint updates applied only after SQLite `COMMIT`.
#[derive(Default)]
pub(crate) struct HotFpDelta {
    chars_upserted: Vec<(i64, u64)>,
    chars_removed: Vec<i64>,
    skills_upserted: Vec<(i64, TypeId, u64)>,
    skills_removed_chars: Vec<i64>,
    skills_removed_rows: Vec<(i64, TypeId)>,
    queues_upserted: Vec<(i64, u64)>,
    queues_removed: Vec<i64>,
    items_upserted: Vec<(i64, u64)>,
    items_removed: Vec<i64>,
    orders_upserted: Vec<(i64, u64)>,
    orders_removed: Vec<i64>,
    next_market_orders_digest: Option<u64>,
}

impl HotFpDelta {
    pub(crate) fn apply_to(self, fp: &mut TableFingerprints) {
        for id in self.chars_removed {
            fp.characters.remove(&id);
        }
        for (id, h) in self.chars_upserted {
            fp.characters.insert(id, h);
        }
        for cid in self.skills_removed_chars {
            fp.skills.remove(&cid);
        }
        for (cid, tid) in self.skills_removed_rows {
            if let Some(m) = fp.skills.get_mut(&cid) {
                m.remove(&tid);
            }
        }
        for (cid, tid, digest) in self.skills_upserted {
            fp.skills.entry(cid).or_default().insert(tid, digest);
        }
        for cid in self.queues_removed {
            fp.skill_queues.remove(&cid);
        }
        for (cid, h) in self.queues_upserted {
            fp.skill_queues.insert(cid, h);
        }
        for id in self.items_removed {
            fp.items.remove(&id);
        }
        for (id, h) in self.items_upserted {
            fp.items.insert(id, h);
        }
        if !self.orders_removed.is_empty() || !self.orders_upserted.is_empty() {
            let fp_orders = Arc::make_mut(&mut fp.market_orders);
            for oid in self.orders_removed {
                fp_orders.remove(&oid);
            }
            for (oid, h) in self.orders_upserted {
                fp_orders.insert(oid, h);
            }
        }
        if let Some(digest) = self.next_market_orders_digest {
            fp.market_orders_digest = digest;
        }
    }
}

/// Applies `changes` inside an already-started SQLite `Transaction` in `O(k)` for `CowMap` hot
/// tables, recording deferred hot fingerprint updates in `HotFpDelta` without cloning hot maps.
pub(crate) fn apply_changeset_to_sqlite(
    exec: &mut TxSqlExecutor<'_, '_>,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    base_fp: &TableFingerprints,
    staged_cold_fp: &mut TableFingerprints,
    need_wal_delta: bool,
) -> Result<(crate::persistence::WorldStateWalDelta, HotFpDelta), TxError> {
    use crate::sqlite_store::fingerprints::{
        fp_character, fp_item, fp_market_order, fp_market_orders_digest_entry, fp_skill_queue,
        to_checked_json_string,
    };

    let mut delta = Box::new(crate::persistence::WorldStateWalDelta {
        seq: snap.wal_seq,
        next_entity_id: snap.next_entity_id,
        ..Default::default()
    });
    let mut hot_fp = HotFpDelta::default();

    if changes.meta_changed {
        exec.execute(
            "world_meta",
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', ?1), ('wal_seq', ?2), ('markets_seeded', ?3)",
            params![
                snap.next_entity_id,
                snap.wal_seq as i64,
                i64::from(snap.markets_seeded)
            ],
        )?;
        staged_cold_fp.next_entity_id = snap.next_entity_id;
        staged_cold_fp.wal_seq = snap.wal_seq;
        staged_cold_fp.markets_seeded = snap.markets_seeded;
    }

    // Hot CowMap tables: O(k) SQL updates directly from ChangeSet!
    for ch in &changes.characters {
        match &ch.after {
            None => {
                exec.execute(
                    "characters:delete",
                    "DELETE FROM characters WHERE character_id = ?1",
                    params![ch.key],
                )?;
                hot_fp.chars_removed.push(ch.key);
            }
            Some(v) => {
                let rec_json = to_checked_json_string(&**v).map_err(TxError::Persist)?;
                exec.execute(
                    "characters:upsert",
                    "INSERT INTO characters (character_id, user_id, name, corporation_id, alliance_id, station_id, structure_id, solar_system_id, active_ship_id, balance_cents, activity_status, version, record_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
                     ON CONFLICT(character_id) DO UPDATE SET
                         user_id = excluded.user_id,
                         name = excluded.name,
                         corporation_id = excluded.corporation_id,
                         alliance_id = excluded.alliance_id,
                         station_id = excluded.station_id,
                         structure_id = excluded.structure_id,
                         solar_system_id = excluded.solar_system_id,
                         active_ship_id = excluded.active_ship_id,
                         balance_cents = excluded.balance_cents,
                         activity_status = excluded.activity_status,
                         version = excluded.version,
                         record_json = excluded.record_json",
                    params![
                        v.character_id,
                        v.user_id as i64,
                        v.name,
                        v.corporation_id,
                        v.alliance_id,
                        v.station_id,
                        v.structure_id,
                        v.solar_system_id as i64,
                        v.active_ship_id,
                        v.balance.0,
                        v.activity_status,
                        v.version as i64,
                        rec_json,
                    ],
                )?;
                hot_fp.chars_upserted.push((ch.key, fp_character(v)));
                if need_wal_delta {
                    delta.upserted_characters.push((**v).clone());
                }
            }
        }
    }

    for ch in &changes.skills {
        match &ch.after {
            None => {
                exec.execute(
                    "skills:delete_char",
                    "DELETE FROM skills WHERE character_id = ?1",
                    params![ch.key],
                )?;
                hot_fp.skills_removed_chars.push(ch.key);
            }
            Some(sk_map) => {
                let empty_map = HashMap::new();
                let fp_map = base_fp.skills.get(&ch.key).unwrap_or(&empty_map);
                let rem_tids: Vec<TypeId> = fp_map
                    .keys()
                    .filter(|tid| !sk_map.contains_key(*tid))
                    .copied()
                    .collect();
                for tid in rem_tids {
                    exec.execute(
                        "skills:delete_row",
                        "DELETE FROM skills WHERE character_id = ?1 AND type_id = ?2",
                        params![ch.key, tid as i64],
                    )?;
                    hot_fp.skills_removed_rows.push((ch.key, tid));
                }
                for (&tid, sk) in sk_map.iter() {
                    let digest = crate::sqlite_store::fingerprints::canonical_fp(sk);
                    if fp_map.get(&tid) != Some(&digest) {
                        exec.execute(
                            "skills:upsert",
                            "INSERT OR REPLACE INTO skills (character_id, type_id, level, skill_points) VALUES (?1, ?2, ?3, ?4)",
                            params![ch.key, tid as i64, sk.level as i64, sk.skill_points],
                        )?;
                        hot_fp.skills_upserted.push((ch.key, tid, digest));
                        if need_wal_delta {
                            delta.upserted_skills.push((ch.key, sk.clone()));
                        }
                    }
                }
            }
        }
    }

    for ch in &changes.skill_queues {
        match &ch.after {
            None => {
                exec.execute(
                    "skill_queues:delete",
                    "DELETE FROM skill_queues WHERE character_id = ?1",
                    params![ch.key],
                )?;
                hot_fp.queues_removed.push(ch.key);
            }
            Some(q) => {
                exec.execute(
                    "skill_queues:delete_old",
                    "DELETE FROM skill_queues WHERE character_id = ?1",
                    params![ch.key],
                )?;
                for (idx, entry) in q.iter().enumerate() {
                    exec.execute(
                        "skill_queues:insert",
                        "INSERT INTO skill_queues (character_id, queue_index, type_id, target_level, start_sp, end_sp, duration_seconds, started_at_unix)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            ch.key,
                            idx as i64,
                            entry.type_id as i64,
                            entry.target_level as i64,
                            entry.start_sp,
                            entry.end_sp,
                            entry.duration_seconds,
                            entry.started_at_unix.map(|v| v as i64),
                        ],
                    )?;
                }
                hot_fp.queues_upserted.push((ch.key, fp_skill_queue(q)));
                if need_wal_delta {
                    delta.upserted_skill_queues.push((ch.key, (**q).clone()));
                }
            }
        }
    }

    for ch in &changes.items {
        match &ch.after {
            None => {
                exec.execute(
                    "items:delete",
                    "DELETE FROM items WHERE item_id = ?1",
                    params![ch.key],
                )?;
                hot_fp.items_removed.push(ch.key);
                if need_wal_delta {
                    delta.removed_items.push(ch.key);
                }
            }
            Some(item) => {
                let rec_json = to_checked_json_string(&**item).map_err(TxError::Persist)?;
                exec.execute(
                    "items:upsert",
                    "INSERT OR REPLACE INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        item.item_id,
                        item.type_id as i64,
                        item.owner_id,
                        item.location_id,
                        item.flag as i64,
                        item.quantity as i64,
                        i64::from(item.singleton),
                        item.version as i64,
                        rec_json,
                    ],
                )?;
                hot_fp.items_upserted.push((ch.key, fp_item(item)));
                if need_wal_delta {
                    delta.upserted_items.push((**item).clone());
                }
            }
        }
    }

    if !changes.market_orders.is_empty() {
        let mut digest = base_fp.market_orders_digest;
        for ch in &changes.market_orders {
            match &ch.after {
                None => {
                    exec.execute(
                        "market_orders:delete",
                        "DELETE FROM market_orders WHERE order_id = ?1",
                        params![ch.key],
                    )?;
                    if let Some(&old_fp) = base_fp.market_orders.get(&ch.key) {
                        digest ^= fp_market_orders_digest_entry(ch.key, old_fp);
                    }
                    hot_fp.orders_removed.push(ch.key);
                    if need_wal_delta {
                        delta.removed_market_orders.push(ch.key);
                    }
                }
                Some(ord) => {
                    exec.execute(
                        "market_orders:upsert",
                        "INSERT OR REPLACE INTO market_orders (order_id, owner_id, station_id, region_id, type_id, is_bid, price_cents, vol_entered, vol_remaining, min_volume, is_npc, escrow_cents)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            ord.order_id,
                            ord.owner_id,
                            ord.station_id,
                            ord.region_id as i64,
                            ord.type_id as i64,
                            i64::from(ord.is_bid),
                            ord.price.0,
                            ord.vol_entered as i64,
                            ord.vol_remaining as i64,
                            ord.min_volume as i64,
                            i64::from(ord.is_npc),
                            ord.escrow.0,
                        ],
                    )?;
                    let new_fp = fp_market_order(ord);
                    if let Some(&old_fp) = base_fp.market_orders.get(&ch.key) {
                        digest ^= fp_market_orders_digest_entry(ch.key, old_fp);
                    }
                    digest ^= fp_market_orders_digest_entry(ch.key, new_fp);
                    hot_fp.orders_upserted.push((ch.key, new_fp));
                    if need_wal_delta {
                        delta.upserted_market_orders.push((**ord).clone());
                    }
                }
            }
        }
        hot_fp.next_market_orders_digest = Some(digest);
    }

    // Selective cold domain SQL sync: never scans the 5 hot CowMap tables!
    let cd = &changes.cold_domains;
    if cd.any() {
        exec.step_fault_check("cold_domains")?;
        crate::sqlite_store::sync::sync_selected_cold_domains_to_sqlite(
            exec.tx,
            snap,
            staged_cold_fp,
            cd,
            &mut delta,
        )
        .map_err(TxError::Persist)?;
    }

    delta.dirty = !changes.is_empty();
    if need_wal_delta && changes.cold_changed {
        delta.full_snapshot = Some(Box::new(snap.clone()));
    }
    Ok((*delta, hot_fp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::WorldRepository;
    use crate::types::{InventoryFlag, InventoryItemRecord, IskCents};
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

    fn make_test_item(item_id: i64, owner_id: i64, qty: i32) -> InventoryItemRecord {
        InventoryItemRecord {
            item_id,
            type_id: 34,
            owner_id,
            location_id: 60_003_760,
            flag: InventoryFlag::Hangar,
            quantity: qty,
            singleton: false,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
        }
    }

    struct TestTempDir(std::path::PathBuf);
    impl TestTempDir {
        fn new(label: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "evomu_tx_engine_{label}_{}_{n}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create test tempdir");
            Self(dir)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn verify_fault_plan_step(fail_kind: u8, stmt_idx: usize, seed_items: usize, delta_qty: i32) {
        let dir = TestTempDir::new("fault_plan");
        let db_path = dir.path().join("fault_plan.sqlite");
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

        // Seed initial committed state: 1 character + `seed_items` items + cold note.
        let cid = 90_000_001_i64;
        repo.tx("seed_base", |wtx| -> Result<(), TxError> {
            wtx.snap.characters.insert(
                cid,
                crate::types::CharacterRecord {
                    character_id: cid,
                    user_id: 1,
                    name: "Pilot".into(),
                    balance: IskCents(100_000),
                    version: 1,
                    ..Default::default()
                },
            );
            for _i in 0..seed_items {
                let id = wtx.alloc_entity_id()?;
                wtx.snap.items.insert(id, make_test_item(id, cid, 10));
            }
            wtx.snap.reconcile_id_counters();
            Ok(())
        })
        .expect("seed_base");

        let before = repo.read_state();
        let notifications_seen = Arc::new(AtomicUsize::new(0));
        let notifications_hook = Arc::clone(&notifications_seen);
        repo.register_post_commit_listener(Arc::new(move |_snap, _ch, eff| {
            notifications_hook.fetch_add(eff.notifications.len(), AtomicOrdering::SeqCst);
        }));

        let fault_res: Result<(), TxError> = match fail_kind % 3 {
            0 => {
                repo.set_fault_plan_for_test(TxFaultPlan::FailNthStatement(stmt_idx.clamp(1, 5)));
                repo.tx("fault_stmt", |wtx| {
                    let nid1 = wtx.alloc_entity_id()?;
                    let nid2 = wtx.alloc_entity_id()?;
                    wtx.snap.items.insert(nid1, make_test_item(nid1, cid, delta_qty));
                    wtx.snap.items.insert(nid2, make_test_item(nid2, cid, delta_qty + 1));
                    if let Some(ch) = wtx.snap.characters.get_mut(&cid) {
                        ch.balance = IskCents(999_999);
                        ch.version += 1;
                    }
                    wtx.snap.owner_notes.insert(
                        1,
                        crate::types::OwnerNoteRecord {
                            note_id: 1,
                            character_id: cid,
                            item_id: nid1,
                            label: "FaultStmt".into(),
                            note: "Must roll back".into(),
                            timestamp_unix: 1,
                        },
                    );
                    wtx.effects.notify_character(cid, "OnItemChange", "charid", vec![]);
                    Ok(())
                })
            }
            1 => {
                repo.set_fault_plan_for_test(TxFaultPlan::FailAtCommit);
                repo.tx("fault_commit", |wtx| {
                    let nid = wtx.alloc_entity_id()?;
                    wtx.snap.items.insert(nid, make_test_item(nid, cid, delta_qty));
                    wtx.snap.owner_notes.insert(
                        1,
                        crate::types::OwnerNoteRecord {
                            note_id: 1,
                            character_id: cid,
                            item_id: 1,
                            label: "Abort".into(),
                            note: "Must roll back".into(),
                            timestamp_unix: 1,
                        },
                    );
                    wtx.effects.notify_character(cid, "OnNoteAdd", "charid", vec![]);
                    Ok(())
                })
            }
            _ => repo.tx("fault_panic", |wtx| {
                let nid = wtx.alloc_entity_id()?;
                wtx.snap.items.insert(nid, make_test_item(nid, cid, delta_qty));
                wtx.effects.notify_character(cid, "OnPanicDrop", "charid", vec![]);
                panic!("fault-plan closure panic");
            }),
        };
        assert!(fault_res.is_err(), "fault-injected tx must return Err");
        assert_eq!(
            notifications_seen.load(AtomicOrdering::SeqCst),
            0,
            "aborted/failed/panicked tx must never deliver post-commit effects"
        );

        let published = repo.read_state();
        let fresh_load = repo.reload_snapshot_from_sqlite().expect("fresh_load");
        assert_eq!(*published, *before, "published snapshot must equal before");
        assert_eq!(fresh_load, *before, "SQLite fresh_load must equal before");
        drop(repo);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]
        #[test]
        fn tx_fault_plan_property_before_equals_published_equals_fresh_load(
            fail_kind in 0u8..3,
            stmt_idx in 1usize..6,
            seed_items in 0usize..4,
            delta_qty in 1i32..500,
        ) {
            verify_fault_plan_step(fail_kind, stmt_idx, seed_items, delta_qty);
        }
    }

    #[test]
    fn tx_id_allocator_never_reissues_after_abort_or_restart() {
        let dir = TestTempDir::new("id_non_reuse");
        let db_path = dir.path().join("id_non_reuse.sqlite");

        let burned_ids: Vec<i64>;
        {
            let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
            let mut allocated = Vec::new();
            let err: Result<(), TxError> = repo.tx("aborted_id_alloc", |wtx| {
                allocated.push(wtx.alloc_entity_id()?);
                allocated.push(wtx.alloc_entity_id()?);
                allocated.push(wtx.alloc_entity_id()?);
                Err(TxError::Persist("intentional abort after ID alloc".into()))
            });
            assert!(err.is_err());
            burned_ids = allocated;
            assert_eq!(burned_ids.len(), 3);

            // Next committed tx in the same process must never reissue burned IDs.
            let next_id = repo
                .tx("commit_after_abort", |wtx| -> Result<i64, TxError> {
                    wtx.alloc_entity_id()
                })
                .expect("commit_after_abort");
            assert!(
                next_id > *burned_ids.last().unwrap(),
                "next_id ({next_id}) must exceed burned IDs {burned_ids:?}"
            );
        }

        // Reopen DB (simulating process restart) and allocate again — burned IDs and prior committed
        // IDs must NEVER be reissued.
        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen sqlite");
        let after_restart = reopened
            .tx("post_restart_alloc", |wtx| -> Result<i64, TxError> {
                wtx.alloc_entity_id()
            })
            .expect("post_restart_alloc");
        for &burned in &burned_ids {
            assert!(
                after_restart > burned,
                "post-restart ID {after_restart} reissued or regressed below burned ID {burned}"
            );
        }
    }

    #[test]
    fn tx_post_commit_effects_fire_in_order_and_drop_on_abort() {
        let repo = WorldRepository::with_demo_seed();
        let events = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let events_listener = Arc::clone(&events);
        repo.register_post_commit_listener(Arc::new(move |_snap, ch, eff| {
            let mut guard = events_listener.lock().unwrap();
            for n in &eff.notifications {
                guard.push(format!("seq={}:{}", ch.seq, n.event));
            }
        }));

        let abort_res: Result<(), TxError> = repo.tx("abort_no_effect", |wtx| {
            wtx.effects.notify_character(90_000_001, "Dropped", "charid", vec![]);
            let ev_cb = Arc::clone(&events);
            wtx.effects.on_commit(move |_| {
                ev_cb.lock().unwrap().push("should_not_fire".into());
            });
            Err(TxError::Persist("abort".into()))
        });
        assert!(abort_res.is_err());
        assert!(events.lock().unwrap().is_empty());

        repo.tx("commit_first", |wtx| -> Result<(), TxError> {
            wtx.effects.notify_character(90_000_001, "First", "charid", vec![]);
            Ok(())
        })
        .expect("commit_first");
        repo.tx("commit_second", |wtx| -> Result<(), TxError> {
            wtx.effects.notify_character(90_000_001, "Second", "charid", vec![]);
            Ok(())
        })
        .expect("commit_second");

        assert_eq!(
            *events.lock().unwrap(),
            vec!["seq=1:First".to_string(), "seq=1:Second".to_string()]
        );
    }

    #[test]
    fn tx_1m_items_commit_time_independent_of_world_size() {
        let build_repo_with_items = |count: usize| -> WorldRepository {
            let mut conn = rusqlite::Connection::open_in_memory().expect("open_in_memory");
            crate::sqlite_store::apply_migrations(&conn).expect("migrations");
            let mut snap = WorldStateSnapshot::default();
            let mut fp = crate::sqlite_store::TableFingerprints::default();
            let tx = conn.transaction().expect("seed tx");
            {
                let mut stmt = tx
                    .prepare_cached(
                        "INSERT INTO items (item_id, type_id, owner_id, location_id, flag, quantity, singleton, version, record_json)
                         VALUES (?1, 34, 90000001, 60003760, 4, 1, 0, 1, '')",
                    )
                    .expect("prepare");
                for i in 0..count {
                    let id = 200_100_000 + i as i64;
                    let item = make_test_item(id, 90_000_001, 1);
                    stmt.execute(rusqlite::params![id]).expect("seed sql item");
                    fp.items.insert(id, crate::sqlite_store::fingerprints::fp_item(&item));
                    snap.items.insert(id, item);
                }
            }
            tx.commit().expect("seed commit");
            let jwt_secret = crate::sqlite_store::load_or_init_jwt_secret(
                &conn,
                WorldRepository::generate_random_secret_32,
            )
            .expect("jwt");
            WorldRepository::from_parts(
                snap,
                conn,
                fp,
                200_100_000 + count as i64,
                None,
                crate::types::AuthRuntimeState::new(jwt_secret),
            )
        };

        let small_repo = build_repo_with_items(1_000);
        let small_t0 = Instant::now();
        for i in 0..20 {
            let id = 300_000_000 + i;
            small_repo
                .tx("small_1_item", |wtx| -> Result<(), TxError> {
                    wtx.snap.items.insert(id, make_test_item(id, 90_000_001, 5));
                    Ok(())
                })
                .expect("small commit");
        }
        let small_avg = small_t0.elapsed() / 20;

        let large_repo = build_repo_with_items(1_000_000);
        assert_eq!(large_repo.read_state().items.len(), 1_000_000);
        let large_t0 = Instant::now();
        for i in 0..20 {
            let id = 300_000_000 + i;
            large_repo
                .tx("large_1m_1_item", |wtx| -> Result<(), TxError> {
                    wtx.snap.items.insert(id, make_test_item(id, 90_000_001, 5));
                    Ok(())
                })
                .expect("large commit");
        }
        let large_avg = large_t0.elapsed() / 20;

        assert!(
            large_avg < std::time::Duration::from_millis(50),
            "1M-item single-row commit took {large_avg:?} (expected O(log N) << 50ms; 1k avg was {small_avg:?})"
        );
    }
}
