//! Copy-on-Write transaction engine, diffing (`ChangeSet`), post-commit `Effects`,
//! external block-persisted `EntityIdAllocator`, and SQLite fault-plan injection (`RFC-2 §3.2–§3.4`).

use crate::cow::CowDiffItem;
use crate::sqlite_store::checked_json::{to_checked_json_string, FiniteFloatChecker};
use crate::types::{
    CharacterRecord, DbError, InventoryItemRecord, MarketOrderRecord, SkillQueueEntry,
    TrainedSkillRecord, TypeId, WorldStateSnapshot,
};
use rusqlite::{params, Connection, Transaction};
use serde::Serialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

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
            client_error: None,
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
    pub d6_notes_lp_seq: bool,
    pub d6_social: bool,
    pub d6_corp_crimewatch: bool,
    pub d6_batch4: bool,
    pub service_domains: bool,
    pub crimewatch_changed: bool,
    pub combat_runtime_changed: bool,
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
    pub wallet_journal_diff: crate::cow_collections::CowVecDiff,
    pub mails_diff: crate::cow_collections::CowVecDiff,
    pub structures: Vec<RowChange<i64, crate::types::UpwellStructureRecord>>,
    pub mail_timestamps: Vec<RowChange<i64, u64>>,
    pub item_custom_names: Vec<RowChange<i64, String>>,
    pub entity_kv: Vec<
        RowChange<
            crate::service_domains::EntityKvKey,
            crate::service_domains::EntityKvRow,
        >,
    >,
    pub domain_rows: Vec<crate::sqlite_store::domain_rows::DomainRowChange>,
    pub cold_changed: bool,
    pub cold_domains: ColdDomainDiff,
}

impl ChangeSet {
    /// Returns `true` when zero metadata, hot-table, `domain_rows`, or cold-domain changes occurred.
    #[inline]
    pub fn is_empty(&self) -> bool {
        !self.meta_changed
            && self.items.is_empty()
            && self.characters.is_empty()
            && self.skills.is_empty()
            && self.skill_queues.is_empty()
            && self.market_orders.is_empty()
            && matches!(
                self.wallet_journal_diff,
                crate::cow_collections::CowVecDiff::Unchanged
            )
            && matches!(
                self.mails_diff,
                crate::cow_collections::CowVecDiff::Unchanged
            )
            && self.structures.is_empty()
            && self.mail_timestamps.is_empty()
            && self.item_custom_names.is_empty()
            && self.entity_kv.is_empty()
            && self.domain_rows.is_empty()
            && !self.cold_changed
    }

    /// Computes a `ChangeSet` between `before` and `after` snapshots (`O(k log N)` for `CowMap`s).
    pub fn diff(seq: u64, before: &WorldStateSnapshot, after: &WorldStateSnapshot) -> Self {
        use crate::sqlite_store::domain_rows::{
            Persist, DOMAIN_ABYSSAL_STATE, DOMAIN_ACCOUNT_PLEX_VAULT, DOMAIN_CHAR_LP_LOGS,
            DOMAIN_CLUSTER_AND_RUNTIME, DOMAIN_CORP_LP_BALANCES, DOMAIN_CRAB_BEACONS,
            DOMAIN_DEPLOYABLE_STATE, DOMAIN_DUNGEON_EDITOR, DOMAIN_DUNGEON_RUNTIME,
            DOMAIN_FLEET_AND_SOCIAL_OPS, DOMAIN_FOB_OBSERVATORY_STATE,
            DOMAIN_INSURGENCY_COMPLEX_OPS, DOMAIN_MARKET_TRANSACTIONS, DOMAIN_MINING_AND_ESS_STATE,
            DOMAIN_NODE_GRAPH_STATE, DOMAIN_OWNER_NOTES, DOMAIN_PVE_SPECIAL_SITES,
            DOMAIN_PVP_FILAMENT_STATE, DOMAIN_QUASAR_COMMERCE, DOMAIN_SEASONS_AND_CAMPAIGNS,
            DOMAIN_SP6_LIFECYCLE, DOMAIN_SUPERWEAPON_RUNTIME, DOMAIN_TOURNEY_STATE,
            DOMAIN_WALLET_JOURNAL_META,
        };

        let meta_changed = before.next_entity_id != after.next_entity_id
            || before.wal_seq != after.wal_seq
            || before.markets_seeded != after.markets_seeded;

        let items = diff_cow_map(&before.items, &after.items);
        let characters = diff_cow_map(&before.characters, &after.characters);
        let skills = diff_cow_map(&before.skills, &after.skills);
        let skill_queues = diff_cow_map(&before.skill_queues, &after.skill_queues);
        let market_orders = diff_cow_map(&before.market_orders, &after.market_orders);
        let wallet_journal_diff = before.wallet_journal.classify_diff(&after.wallet_journal);
        let mails_diff = before.mails.classify_diff(&after.mails);
        let structures = diff_cow_map(&before.structures, &after.structures);
        let mail_timestamps = diff_cow_map(&before.mail_timestamps, &after.mail_timestamps);
        let item_custom_names = diff_cow_map(&before.item_custom_names, &after.item_custom_names);
        let entity_kv = diff_cow_map(&before.entity_kv, &after.entity_kv);

        let mut domain_rows = Vec::new();
        macro_rules! push_domain_diff {
            ($($field:ident => $domain:ident),+ $(,)?) => {
                $(
                    before.$field.collect_diff_ops(&after.$field, $domain, &mut domain_rows);
                )+
            };
        }
        macro_rules! push_cow_domain_diff {
            ($($field:ident => $domain:ident),+ $(,)?) => {
                $(
                    if !before.$field.ptr_eq(&after.$field) {
                        (*before.$field).collect_diff_ops(&*after.$field, $domain, &mut domain_rows);
                    }
                )+
            };
        }
        push_domain_diff!(
            wallet_journal_meta => DOMAIN_WALLET_JOURNAL_META,
            market_transactions => DOMAIN_MARKET_TRANSACTIONS,
            account_plex_vault => DOMAIN_ACCOUNT_PLEX_VAULT,
            owner_notes => DOMAIN_OWNER_NOTES,
            corp_lp_balances => DOMAIN_CORP_LP_BALANCES,
            char_lp_logs => DOMAIN_CHAR_LP_LOGS,
        );
        push_cow_domain_diff!(
            pve_special_sites => DOMAIN_PVE_SPECIAL_SITES,
            abyssal_state => DOMAIN_ABYSSAL_STATE,
            dungeon_runtime => DOMAIN_DUNGEON_RUNTIME,
            dungeon_editor => DOMAIN_DUNGEON_EDITOR,
            crab_beacons => DOMAIN_CRAB_BEACONS,
            fob_observatory_state => DOMAIN_FOB_OBSERVATORY_STATE,
            mining_and_ess_state => DOMAIN_MINING_AND_ESS_STATE,
            pvp_filament_state => DOMAIN_PVP_FILAMENT_STATE,
            tourney_state => DOMAIN_TOURNEY_STATE,
            deployable_state => DOMAIN_DEPLOYABLE_STATE,
            superweapon_runtime => DOMAIN_SUPERWEAPON_RUNTIME,
            insurgency_complex_ops => DOMAIN_INSURGENCY_COMPLEX_OPS,
            node_graph_state => DOMAIN_NODE_GRAPH_STATE,
            seasons_and_campaigns => DOMAIN_SEASONS_AND_CAMPAIGNS,
            quasar_commerce => DOMAIN_QUASAR_COMMERCE,
            fleet_and_social_ops => DOMAIN_FLEET_AND_SOCIAL_OPS,
            cluster_and_runtime => DOMAIN_CLUSTER_AND_RUNTIME,
            sp6_lifecycle => DOMAIN_SP6_LIFECYCLE,
        );
        crate::sqlite_store::domain_rows::collect_social_domain_row_diffs(
            before,
            after,
            &mut domain_rows,
        );
        crate::sqlite_store::domain_rows::collect_corp_crimewatch_domain_row_diffs(
            before,
            after,
            &mut domain_rows,
        );
        crate::sqlite_store::domain_rows::collect_industry_market_pve_domain_row_diffs(
            before,
            after,
            &mut domain_rows,
        );

        let cold_domains = compute_cold_domain_diff(before, after);
        let cold_changed = cold_domains.any()
            || before.markets_seeded != after.markets_seeded
            || items.iter().any(|c| c.after.is_none())
            || characters.iter().any(|c| c.after.is_none())
            || skills.iter().any(|c| c.after.is_none())
            || skill_queues.iter().any(|c| c.after.is_none())
            || market_orders.iter().any(|c| c.after.is_none())
            || matches!(
                wallet_journal_diff,
                crate::cow_collections::CowVecDiff::Rewritten
            )
            || !matches!(mails_diff, crate::cow_collections::CowVecDiff::Unchanged)
            || !structures.is_empty()
            || !mail_timestamps.is_empty()
            || !item_custom_names.is_empty()
            || !entity_kv.is_empty();

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
            wallet_journal_diff,
            mails_diff,
            structures,
            mail_timestamps,
            item_custom_names,
            entity_kv,
            domain_rows,
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
        || before.saved_fittings != after.saved_fittings;

    let part2_activity = before.contracts != after.contracts
        || before.industry_jobs != after.industry_jobs
        || before.planetary_colonies != after.planetary_colonies
        || before.active_missions != after.active_missions
        || before.mission_tally != after.mission_tally
        || before.standing_transactions != after.standing_transactions
        || before.npe_progress != after.npe_progress
        || before.bookmarks != after.bookmarks;

    let part3_social_corp = before.fleets != after.fleets
        || before.corporations != after.corporations
        || before.alliances != after.alliances
        || before.wars != after.wars
        || before.corp_projects != after.corp_projects
        || before.killmails != after.killmails
        || before.kill_rights != after.kill_rights;

    let part4_world_tail = before.system_states != after.system_states
        || before.committed_idempotency_keys != after.committed_idempotency_keys
        || before.licensed_skins != after.licensed_skins
        || before.applied_ship_skins != after.applied_ship_skins
        || before.asteroids != after.asteroids;

    let extension_tables = before.character_last_jump_unix != after.character_last_jump_unix
        || before.character_cspa_charges != after.character_cspa_charges
        || before.character_login_stats != after.character_login_stats
        || before.item_positions != after.item_positions
        || before.structure_vulnerability_windows != after.structure_vulnerability_windows
        || before.skill_history != after.skill_history
        || before.character_doll_info != after.character_doll_info;

    // Family 7 (`sp6_lifecycle`) now persists via incremental `domain_rows` diffs;
    // only the four `world_meta` sequence counters remain on `D-6`.
    let d6_notes_lp_seq = before.next_alliance_id != after.next_alliance_id
        || before.next_corporation_id != after.next_corporation_id
        || before.next_notification_id != after.next_notification_id
        || before.next_report_id != after.next_report_id;

    // Family 3 (`social`) now persists via incremental `domain_rows` diffs.
    let d6_social = false;

    let combat_runtime_changed =
        before.combat_crimewatch_state != after.combat_crimewatch_state;
    let crimewatch_changed = before.crimewatch_states != after.crimewatch_states
        || before.crimewatch_aggressions != after.crimewatch_aggressions
        || combat_runtime_changed;

    // Family 4 (`corp_crimewatch`) now persists via incremental `domain_rows` diffs.
    let d6_corp_crimewatch = false;

    // Family 5 (`industry_market_pve`) migrated its 34 `world_kv_blobs` collections into
    // `domain_rows`; only `char_mining_ledger` remains a relational cold table here.
    let d6_batch4 = before.mining_ledger != after.mining_ledger;

    // `entity_kv` now persists via incremental `ChangeSet::entity_kv` (`CowMap` diff).
    let service_domains = false;

    ColdDomainDiff {
        part1_cold,
        part2_activity,
        part3_social_corp,
        part4_world_tail,
        extension_tables,
        d6_notes_lp_seq,
        d6_social,
        d6_corp_crimewatch,
        d6_batch4,
        service_domains,
        crimewatch_changed,
        combat_runtime_changed,
    }
}

struct ByteCountingWriter {
    bytes: usize,
}

impl std::io::Write for ByteCountingWriter {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buf.len());
        Ok(buf.len())
    }

    #[inline]
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
    let mut counter = ByteCountingWriter { bytes: 0 };
    serde_json::to_writer(&mut counter, val).map_err(|e| TxError::Persist(e.to_string()))?;
    let bytes = counter.bytes;
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
    match changes.wallet_journal_diff {
        crate::cow_collections::CowVecDiff::Unchanged => {}
        crate::cow_collections::CowVecDiff::AppendOnly { old_len } => {
            for (idx, e) in after.wallet_journal.iter_from(old_len) {
                validate_row("wallet_journal", idx.to_string(), e, max_row_bytes)?;
            }
        }
        crate::cow_collections::CowVecDiff::Rewritten => {
            for (idx, e) in after.wallet_journal.iter().enumerate() {
                validate_row("wallet_journal", idx.to_string(), e, max_row_bytes)?;
            }
        }
    }
    match changes.mails_diff {
        crate::cow_collections::CowVecDiff::Unchanged => {}
        crate::cow_collections::CowVecDiff::AppendOnly { old_len } => {
            for (idx, m) in after.mails.iter_from(old_len) {
                validate_row("mails", idx.to_string(), m, max_row_bytes)?;
            }
        }
        crate::cow_collections::CowVecDiff::Rewritten => {
            for (idx, m) in after.mails.iter().enumerate() {
                validate_row("mails", idx.to_string(), m, max_row_bytes)?;
            }
        }
    }
    for c in &changes.structures {
        if let Some(ref s) = c.after {
            validate_row("structures", c.key.to_string(), &**s, max_row_bytes)?;
        }
    }
    for c in &changes.item_custom_names {
        if let Some(ref name) = c.after {
            validate_row("item_custom_names", c.key.to_string(), &**name, max_row_bytes)?;
        }
    }
    for c in &changes.entity_kv {
        if let Some(ref row) = c.after {
            validate_row("entity_kv", c.key.to_map_key_string(), &**row, max_row_bytes)?;
        }
    }
    crate::sqlite_store::domain_rows::validate_domain_row_changes(
        &changes.domain_rows,
        max_row_bytes,
    )?;

    let cd = &changes.cold_domains;
    if cd.part1_cold && !before.saved_fittings.ptr_eq(&after.saved_fittings) {
        for (k, v) in &after.saved_fittings {
            if before.saved_fittings.get(k) != Some(v) {
                validate_row("saved_fittings", k.to_string(), v, max_row_bytes)?;
            }
        }
    }
    if cd.part2_activity {
        if !before.contracts.ptr_eq(&after.contracts) {
            for (k, v) in &after.contracts {
                if before.contracts.get(k) != Some(v) {
                    validate_row("contracts", k.to_string(), v, max_row_bytes)?;
                }
            }
        }
        if !before.industry_jobs.ptr_eq(&after.industry_jobs) {
            for (k, v) in &after.industry_jobs {
                if before.industry_jobs.get(k) != Some(v) {
                    validate_row("industry_jobs", k.to_string(), v, max_row_bytes)?;
                }
            }
        }
        if !before.bookmarks.ptr_eq(&after.bookmarks) {
            for (k, v) in &after.bookmarks {
                if before.bookmarks.get(k) != Some(v) {
                    validate_row("bookmarks", k.to_string(), v, max_row_bytes)?;
                }
            }
        }
    }
    if cd.part4_world_tail {
        if !before.system_states.ptr_eq(&after.system_states) {
            for (k, v) in &after.system_states {
                if before.system_states.get(k) != Some(v) {
                    validate_row("system_states", k.to_string(), v, max_row_bytes)?;
                }
            }
        }
        if !before.asteroids.ptr_eq(&after.asteroids) {
            for (k, v) in &after.asteroids {
                if before.asteroids.get(k) != Some(v) {
                    validate_row("asteroids", k.to_string(), v, max_row_bytes)?;
                }
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
    pub acting_char_id: Option<i64>,
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
    /// Only retains out-of-order referenced IDs strictly above `next`.
    referenced_ids: Mutex<BTreeSet<i64>>,
}

impl EntityIdAllocator {
    /// Creates an allocator starting at `initial_next`, with `persisted_high_water` already
    /// durably recorded in `world_meta.next_entity_block`.
    pub fn new(initial_next: i64, persisted_high_water: i64) -> Self {
        let floor = initial_next.max(200_000_000);
        Self {
            next: AtomicI64::new(floor),
            persisted_high_water: AtomicI64::new(persisted_high_water.max(200_000_000)),
            referenced_ids: Mutex::new(BTreeSet::new()),
        }
    }

    fn prune_referenced_locked(guard: &mut BTreeSet<i64>, watermark: i64) {
        while guard.first().is_some_and(|&v| v <= watermark) {
            guard.pop_first();
        }
    }

    fn claim_next_candidate(&self) -> i64 {
        let mut guard = self
            .referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        loop {
            let id = self.next.fetch_add(1, Ordering::AcqRel) + 1;
            let hit = guard.remove(&id);
            Self::prune_referenced_locked(&mut guard, id);
            if hit {
                continue;
            }
            return id;
        }
    }

    /// Replaces the tracked cross-table referenced ID set on boot or snapshot reload (`#1087`),
    /// retaining only IDs strictly above the monotonic `next` watermark.
    pub fn replace_referenced_ids(&self, ids: HashSet<i64>) {
        let wm = self.next.load(Ordering::Acquire);
        *self
            .referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = ids.into_iter().filter(|&id| id > wm).collect();
    }

    /// Adds additional referenced IDs strictly above `next` to the cross-table skip set (`#1087`).
    pub fn seed_referenced_ids(&self, ids: impl IntoIterator<Item = i64>) {
        let mut guard = self
            .referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let wm = self.next.load(Ordering::Acquire);
        Self::prune_referenced_locked(&mut guard, wm);
        for id in ids {
            self.mark_referenced_locked(&mut guard, id);
        }
    }

    /// Returns `true` if `candidate` is recorded in the out-of-order referenced ID set (`#1087`).
    #[inline]
    pub fn is_referenced(&self, candidate: i64) -> bool {
        self.referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&candidate)
    }

    fn mark_referenced_locked(&self, guard: &mut BTreeSet<i64>, id: i64) {
        let mut wm = self.next.load(Ordering::Acquire);
        Self::prune_referenced_locked(guard, wm);
        if id <= wm {
            return;
        }
        if id == wm + 1 {
            loop {
                match self.next.compare_exchange(
                    wm,
                    wm + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let mut new_wm = wm + 1;
                        while let Some(&first) = guard.first() {
                            if first <= new_wm {
                                guard.pop_first();
                            } else if first == new_wm + 1 {
                                if self
                                    .next
                                    .compare_exchange(
                                        new_wm,
                                        new_wm + 1,
                                        Ordering::AcqRel,
                                        Ordering::Acquire,
                                    )
                                    .is_ok()
                                {
                                    guard.pop_first();
                                    new_wm += 1;
                                } else {
                                    new_wm = self.next.load(Ordering::Acquire);
                                }
                            } else {
                                break;
                            }
                        }
                        return;
                    }
                    Err(observed) => {
                        wm = observed;
                        Self::prune_referenced_locked(guard, wm);
                        if id <= wm {
                            return;
                        }
                        if id > wm + 1 {
                            guard.insert(id);
                            return;
                        }
                    }
                }
            }
        } else {
            guard.insert(id);
        }
    }

    /// Records a referenced ID in `O(1)` memory: ignores IDs already covered by `next`, advances
    /// `next` directly for contiguous IDs, and only stores out-of-order IDs above `next + 1`.
    pub fn mark_referenced(&self, id: i64) {
        let mut guard = self
            .referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.mark_referenced_locked(&mut guard, id);
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
    #[cfg(any(test, feature = "legacy-json-snapshot"))]
    pub(crate) fn reset_from_load(&self, next: i64, persisted_high_water: i64) {
        let bounded = next.max(200_000_000);
        let mut guard = self
            .referenced_ids
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.next.store(bounded, Ordering::Release);
        self.persisted_high_water
            .store(persisted_high_water.max(200_000_000), Ordering::Release);
        Self::prune_referenced_locked(&mut guard, bounded);
    }

    /// Allocates a strictly monotonic entity ID, skipping any candidate still referenced in
    /// cross-table SQLite records or the tracked referenced set, and immediately persisting the
    /// next block of [`ENTITY_ID_BLOCK_SIZE`] into `world_meta.next_entity_block` in autocommit mode.
    pub fn alloc_with_conn(&self, conn: &Connection) -> Result<i64, String> {
        loop {
            let id = self.claim_next_candidate();
            if crate::sqlite_store::sqlite_entity_id_exists(conn, id)? {
                continue;
            }
            self.ensure_block_persisted(conn, id)?;
            return Ok(id);
        }
    }

    /// Allocates a strictly monotonic entity ID while also consulting `snap` for uncommitted or
    /// non-SQLite domain IDs (`#1087`).
    pub fn alloc_with_conn_and_snap(
        &self,
        conn: &Connection,
        snap: &WorldStateSnapshot,
    ) -> Result<i64, String> {
        loop {
            let id = self.claim_next_candidate();
            if snap.is_entity_id_referenced(id) {
                continue;
            }
            self.ensure_block_persisted(conn, id)?;
            return Ok(id);
        }
    }

    /// Synchronously commits both `next_entity_id` and `next_entity_block` to `world_meta`
    /// (used at identity/account/character boundaries so exact counter values survive crash) (`#1087`).
    pub fn commit_counter_sync(&self, conn: &Connection, floor: i64) -> Result<i64, String> {
        let mut cur = self.next.load(Ordering::Acquire);
        let floor = floor.max(200_000_000);
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
        let committed = self.next.load(Ordering::Acquire).max(floor);
        {
            let mut guard = self
                .referenced_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Self::prune_referenced_locked(&mut guard, committed);
        }
        let block = self
            .persisted_high_water
            .load(Ordering::Acquire)
            .max(committed);
        conn.execute(
            "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', ?1), ('next_entity_block', ?2)",
            params![committed, block],
        )
        .map_err(|e| e.to_string())?;
        let mut prev = self.persisted_high_water.load(Ordering::Acquire);
        while prev < block {
            match self.persisted_high_water.compare_exchange_weak(
                prev,
                block,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => prev = observed,
            }
        }
        Ok(committed)
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
        {
            let wm = self.next.load(Ordering::Acquire);
            let mut guard = self
                .referenced_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            Self::prune_referenced_locked(&mut guard, wm);
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
    pub(crate) clock_offset_secs: Option<u64>,
    pub(crate) unrebased_clock_offset_secs: Option<u64>,
}

impl<'a> WorldTx<'a> {
    /// Schedules `world_meta.clock_offset_secs` to be written in the same SQLite transaction (`#920`, `#864`).
    pub fn set_clock_offset_secs(&mut self, offset_secs: u64) {
        self.clock_offset_secs = Some(offset_secs);
    }

    /// Schedules `world_meta.unrebased_clock_offset_secs` to be written in the same SQLite transaction (`#1061`).
    pub fn set_unrebased_clock_offset_secs(&mut self, offset_secs: u64) {
        self.unrebased_clock_offset_secs = Some(offset_secs);
    }

    /// Allocates a strictly monotonic entity ID from the external [`EntityIdAllocator`].
    ///
    /// Aborting or panicking after calling this method still burns the allocated ID and keeps the
    /// persisted `world_meta.next_entity_block` high-water mark so IDs are never reissued.
    pub fn alloc_entity_id(&mut self) -> Result<i64, TxError> {
        if self.snap.active_missions.is_dirty_ids() {
            self.snap.active_missions.reconcile_id_index();
        }
        if self.snap.char_lp_logs.is_dirty_ids() {
            self.snap.char_lp_logs.reconcile_id_index();
        }
        if self.snap.next_entity_id > self.entity_ids.current() {
            self.entity_ids
                .advance_to(self.conn, self.snap.next_entity_id)
                .map_err(TxError::Persist)?;
        }
        let id = self
            .entity_ids
            .alloc_with_conn_and_snap(self.conn, self.snap)
            .map_err(TxError::Persist)?;
        self.snap.next_entity_id = self.snap.next_entity_id.max(id);
        Ok(id)
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

#[cfg(any(test, feature = "legacy-json-snapshot"))]
pub(crate) type ApplyChangesetOutput = crate::persistence::WorldStateWalDelta;
#[cfg(not(any(test, feature = "legacy-json-snapshot")))]
pub(crate) type ApplyChangesetOutput = ();

/// Applies `changes` inside an already-started SQLite `Transaction` in `O(k)` for `CowMap` hot
/// tables and `domain_rows` (`#826`, `#915`).
pub(crate) fn apply_changeset_to_sqlite(
    exec: &mut TxSqlExecutor<'_, '_>,
    before: &WorldStateSnapshot,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    #[cfg(any(test, feature = "legacy-json-snapshot"))] need_wal_delta: bool,
) -> Result<ApplyChangesetOutput, TxError> {
    #[cfg(any(test, feature = "legacy-json-snapshot"))]
    let mut delta = Box::new(crate::persistence::WorldStateWalDelta {
        seq: snap.wal_seq,
        next_entity_id: snap.next_entity_id,
        ..Default::default()
    });

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
    }

    // Hot CowMap tables: O(k) SQL updates directly from ChangeSet with zero row scanning!
    for ch in &changes.characters {
        match &ch.after {
            None => {
                exec.execute(
                    "characters:delete",
                    "DELETE FROM characters WHERE character_id = ?1",
                    params![ch.key],
                )?;
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
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
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
            }
            Some(sk_map) => {
                let before_map = ch.before.as_deref();
                if let Some(prev) = before_map {
                    let mut rem_tids: Vec<TypeId> = prev
                        .keys()
                        .filter(|tid| !sk_map.contains_key(*tid))
                        .copied()
                        .collect();
                    rem_tids.sort_unstable();
                    for tid in rem_tids {
                        exec.execute(
                            "skills:delete_row",
                            "DELETE FROM skills WHERE character_id = ?1 AND type_id = ?2",
                            params![ch.key, tid as i64],
                        )?;
                    }
                }
                let mut upserts: Vec<(TypeId, &TrainedSkillRecord)> = sk_map
                    .iter()
                    .filter(|(tid, sk)| before_map.and_then(|m| m.get(*tid)) != Some(*sk))
                    .map(|(&tid, sk)| (tid, sk))
                    .collect();
                upserts.sort_unstable_by_key(|(tid, _)| *tid);
                for (tid, sk) in upserts {
                    exec.execute(
                        "skills:upsert",
                        "INSERT OR REPLACE INTO skills (character_id, type_id, level, skill_points) VALUES (?1, ?2, ?3, ?4)",
                        params![ch.key, tid as i64, sk.level as i64, sk.skill_points],
                    )?;
                    #[cfg(any(test, feature = "legacy-json-snapshot"))]
                    if need_wal_delta {
                        delta.upserted_skills.push((ch.key, sk.clone()));
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
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
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
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
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
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
                if need_wal_delta {
                    delta.upserted_items.push((**item).clone());
                }
            }
        }
    }

    if !changes.market_orders.is_empty() {
        for ch in &changes.market_orders {
            match &ch.after {
                None => {
                    exec.execute(
                        "market_orders:delete",
                        "DELETE FROM market_orders WHERE order_id = ?1",
                        params![ch.key],
                    )?;
                    #[cfg(any(test, feature = "legacy-json-snapshot"))]
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
                    #[cfg(any(test, feature = "legacy-json-snapshot"))]
                    if need_wal_delta {
                        delta.upserted_market_orders.push((**ord).clone());
                    }
                }
            }
        }
    }

    match changes.wallet_journal_diff {
        crate::cow_collections::CowVecDiff::Unchanged => {}
        crate::cow_collections::CowVecDiff::AppendOnly { old_len } => {
            for (idx, e) in snap.wallet_journal.iter_from(old_len) {
                exec.execute(
                    "wallet_journal:append",
                    "INSERT OR REPLACE INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        idx as i64,
                        e.entry_id,
                        e.character_id,
                        e.entry_type,
                        e.amount.0,
                        e.balance_after.0,
                        e.reason,
                    ],
                )?;
                #[cfg(any(test, feature = "legacy-json-snapshot"))]
                if need_wal_delta {
                    delta.appended_wallet_journal.push(e.clone());
                }
            }
        }
        crate::cow_collections::CowVecDiff::Rewritten => {
            exec.execute(
                "wallet_journal:delete_all",
                "DELETE FROM wallet_journal",
                params![],
            )?;
            for (idx, e) in snap.wallet_journal.iter().enumerate() {
                exec.execute(
                    "wallet_journal:insert",
                    "INSERT INTO wallet_journal (seq_idx, entry_id, character_id, entry_type, amount_cents, balance_after_cents, reason)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        idx as i64,
                        e.entry_id,
                        e.character_id,
                        e.entry_type,
                        e.amount.0,
                        e.balance_after.0,
                        e.reason,
                    ],
                )?;
            }
        }
    }

    match changes.mails_diff {
        crate::cow_collections::CowVecDiff::Unchanged => {}
        crate::cow_collections::CowVecDiff::AppendOnly { old_len } => {
            for (idx, m) in snap.mails.iter_from(old_len) {
                exec.execute(
                    "mail:append",
                    "INSERT OR REPLACE INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        idx as i64,
                        m.message_id,
                        m.sender_id,
                        m.recipient_id,
                        m.subject,
                        m.body,
                    ],
                )?;
            }
        }
        crate::cow_collections::CowVecDiff::Rewritten => {
            exec.execute("mail:delete_all", "DELETE FROM mail", params![])?;
            for (idx, m) in snap.mails.iter().enumerate() {
                exec.execute(
                    "mail:insert",
                    "INSERT INTO mail (seq_idx, message_id, sender_id, recipient_id, subject, body)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        idx as i64,
                        m.message_id,
                        m.sender_id,
                        m.recipient_id,
                        m.subject,
                        m.body,
                    ],
                )?;
            }
        }
    }

    if !changes.structures.is_empty() {
        for ch in &changes.structures {
            match &ch.after {
                None => {
                    exec.execute(
                        "structures:delete",
                        "DELETE FROM structures WHERE structure_id = ?1",
                        params![ch.key],
                    )?;
                }
                Some(st) => {
                    let js = to_checked_json_string(&**st).map_err(TxError::Persist)?;
                    exec.execute(
                        "structures:upsert",
                        "INSERT OR REPLACE INTO structures (structure_id, type_id, owner_corp_id, solar_system_id, name, state, quantum_core_installed, fuel_blocks, shield_hp, armor_hp, hull_hp, record_json)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                        params![
                            st.structure_id,
                            st.type_id as i64,
                            st.owner_corp_id,
                            st.solar_system_id as i64,
                            st.name,
                            st.state,
                            i64::from(st.quantum_core_installed),
                            st.fuel_blocks as i64,
                            st.shield_hp,
                            st.armor_hp,
                            st.hull_hp,
                            js,
                        ],
                    )?;
                }
            }
        }
    }

    if !changes.mail_timestamps.is_empty() {
        for ch in &changes.mail_timestamps {
            match &ch.after {
                None => {
                    exec.execute(
                        "mail_timestamps:delete",
                        "DELETE FROM mail_timestamps WHERE mail_id = ?1",
                        params![ch.key],
                    )?;
                }
                Some(ts) => {
                    exec.execute(
                        "mail_timestamps:upsert",
                        "INSERT OR REPLACE INTO mail_timestamps (mail_id, timestamp_unix) VALUES (?1, ?2)",
                        params![ch.key, **ts as i64],
                    )?;
                }
            }
        }
    }

    if !changes.item_custom_names.is_empty() {
        for ch in &changes.item_custom_names {
            match &ch.after {
                None => {
                    exec.execute(
                        "item_custom_names:delete",
                        "DELETE FROM item_custom_names WHERE item_id = ?1",
                        params![ch.key],
                    )?;
                }
                Some(name) => {
                    exec.execute(
                        "item_custom_names:upsert",
                        "INSERT OR REPLACE INTO item_custom_names (item_id, custom_name) VALUES (?1, ?2)",
                        params![ch.key, &**name],
                    )?;
                }
            }
        }
    }

    if !changes.entity_kv.is_empty() {
        for ch in &changes.entity_kv {
            match &ch.after {
                None => {
                    exec.execute(
                        "entity_kv:delete",
                        "DELETE FROM entity_kv WHERE ns = ?1 AND entity_id = ?2 AND sub_key = ?3",
                        params![ch.key.ns.as_str(), ch.key.entity_id, ch.key.sub_key],
                    )?;
                }
                Some(row) => {
                    crate::service_domains::validate_entity_kv_value(
                        ch.key.ns,
                        &row.value_json,
                    )
                    .map_err(TxError::Persist)?;
                    exec.execute(
                        "entity_kv:upsert",
                        "INSERT OR REPLACE INTO entity_kv (ns, entity_id, sub_key, value, updated_unix) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            ch.key.ns.as_str(),
                            ch.key.entity_id,
                            ch.key.sub_key,
                            row.value_json.as_bytes(),
                            row.updated_unix as i64
                        ],
                    )?;
                }
            }
        }
    }

    if !changes.domain_rows.is_empty() {
        crate::sqlite_store::domain_rows::apply_domain_row_changes(exec, &changes.domain_rows)?;
    }

    // Selective cold domain SQL sync: never scans the 5 hot CowMap tables!
    let cd = &changes.cold_domains;
    if cd.any() {
        exec.step_fault_check("cold_domains")?;
        crate::sqlite_store::sync::sync_selected_cold_domains_to_sqlite(
            exec.tx,
            before,
            snap,
            cd,
            #[cfg(any(test, feature = "legacy-json-snapshot"))]
            need_wal_delta.then_some(&mut *delta),
        )
        .map_err(TxError::Persist)?;
    }

    #[cfg(any(test, feature = "legacy-json-snapshot"))]
    {
        delta.dirty = !changes.is_empty();
        if need_wal_delta && (changes.cold_changed || !changes.domain_rows.is_empty()) {
            delta.full_snapshot = Some(Box::new(snap.clone()));
        }
        Ok(*delta)
    }
    #[cfg(not(any(test, feature = "legacy-json-snapshot")))]
    {
        Ok(())
    }
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

    #[test]
    fn entity_id_alloc_interleaved_wtx_and_snap_across_aborts_is_strictly_monotonic() {
        let dir = TestTempDir::new("bf01_monotonic");
        let db_path = dir.0.join("bf01_monotonic.db");
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

        let mut seen = Vec::new();

        // 1. Committed tx allocating via `wtx.alloc_entity_id()` then `snap.alloc_entity_id()`.
        let (a1, a2) = repo
            .tx("commit_wtx_then_snap", |wtx| -> Result<(i64, i64), TxError> {
                let id1 = wtx.alloc_entity_id()?;
                let id2 = wtx.snap.alloc_entity_id();
                Ok((id1, id2))
            })
            .expect("commit_wtx_then_snap");
        seen.extend([a1, a2]);

        // 2. Aborted tx allocating via both `wtx.alloc_entity_id()` and `wtx.snap.alloc_entity_id()`.
        let abort1: Result<(), TxError> = repo.tx("abort_both", |wtx| {
            let id1 = wtx.alloc_entity_id()?;
            let id2 = wtx.snap.alloc_entity_id();
            assert!(id2 > id1);
            Err(TxError::Persist("intentional abort".into()))
        });
        assert!(abort1.is_err());

        // 3. Committed tx allocating via `wtx.snap.alloc_entity_id()` first, then `wtx.alloc_entity_id()`.
        let (b1, b2) = repo
            .tx("commit_snap_then_wtx", |wtx| -> Result<(i64, i64), TxError> {
                let id1 = wtx.snap.alloc_entity_id();
                let id2 = wtx.alloc_entity_id()?;
                Ok((id1, id2))
            })
            .expect("commit_snap_then_wtx");
        seen.extend([b1, b2]);

        // 4. Aborted tx allocating only via `wtx.snap.alloc_entity_id()` (must still burn/advance).
        let abort2: Result<(), TxError> = repo.tx("abort_snap_only", |wtx| {
            let _ = wtx.snap.alloc_entity_id();
            Err(TxError::Persist("intentional abort 2".into()))
        });
        assert!(abort2.is_err());

        // 5. Committed tx allocating via `wtx.alloc_entity_id()` after snap-only abort.
        let c1 = repo
            .tx("commit_after_snap_abort", |wtx| wtx.alloc_entity_id())
            .expect("commit_after_snap_abort");
        seen.push(c1);

        // 6. Also check read_state snapshot has synced next_entity_id, and restart stays monotonic.
        assert!(repo.read_state().next_entity_id >= c1);
        drop(repo);

        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen_sqlite");
        let (d1, d2) = reopened
            .tx("post_restart_both", |wtx| -> Result<(i64, i64), TxError> {
                let id1 = wtx.snap.alloc_entity_id();
                let id2 = wtx.alloc_entity_id()?;
                Ok((id1, id2))
            })
            .expect("post_restart_both");
        seen.extend([d1, d2]);

        for w in seen.windows(2) {
            assert!(
                w[1] > w[0],
                "entity IDs must be strictly monotonic across wtx/snap interleaving & aborts: {seen:?}"
            );
        }
    }

    #[test]
    fn transient_item_never_persists_to_sqlite_or_wal_and_reclaims_on_unload() {
        // parity: feat:serverops.persistence
        let dir = TestTempDir::new("transient_ns");
        let db_path = dir.path().join("transient_ns.sqlite");
        let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");

        let transient_ship_id = 200_777_001_i64;
        let transient_cargo_id = 200_777_002_i64;
        let sys_id = 30_000_142_u32;

        repo.tx("insert_transient_items", |wtx| -> Result<(), TxError> {
            let mut ship = make_test_item(transient_ship_id, 100_001, 1);
            ship.location_id = i64::from(sys_id);
            ship.singleton = true;
            let mut cargo = make_test_item(transient_cargo_id, 100_001, 50);
            cargo.location_id = transient_ship_id;
            wtx.snap.insert_transient_item(ship);
            wtx.snap.insert_transient_item(cargo);
            Ok(())
        })
        .expect("insert_transient_items");

        let published = repo.read_state();
        assert_eq!(published.wal_seq, 0, "transient insert must not advance wal_seq");
        assert!(published.is_transient_item(transient_ship_id));
        assert!(published.is_transient_item(transient_cargo_id));
        assert!(!published.items.contains_key(&transient_ship_id));

        let json_snap = serde_json::to_string(&*published).expect("serialize snapshot");
        assert!(
            !json_snap.contains("200777001") && !json_snap.contains("200777002"),
            "transient items must be skipped by serde snapshot serialization"
        );

        let sql_count: i64 = repo
            .with_sqlite_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM items WHERE item_id IN (?1, ?2)",
                    params![transient_ship_id, transient_cargo_id],
                    |r| r.get(0),
                )
                .map_err(|e| e.to_string())
            })
            .expect("sqlite count");
        assert_eq!(sql_count, 0, "transient items must never reach SQLite items table");

        // Reclaim on scene unload removes root + child transient items.
        let reclaimed = repo
            .tx("reclaim_scene", |wtx| -> Result<usize, TxError> {
                Ok(wtx.snap.reclaim_transient_items_in_system(sys_id))
            })
            .expect("reclaim_scene");
        assert_eq!(reclaimed, 2);
        assert!(repo.read_state().get_any_item(transient_ship_id).is_none());
        assert!(repo.read_state().get_any_item(transient_cargo_id).is_none());

        // Materialization via death/loot pipeline turns transient item into persistent row.
        let loot_id = 200_777_003_i64;
        repo.tx("seed_and_materialize", |wtx| -> Result<(), TxError> {
            let mut item = make_test_item(loot_id, 100_001, 7);
            item.location_id = i64::from(sys_id);
            wtx.snap.insert_transient_item(item);
            let mat = wtx
                .snap
                .materialize_transient_item(loot_id)
                .expect("materialize");
            mat.location_id = 60_003_760;
            Ok(())
        })
        .expect("seed_and_materialize");
        assert!(!repo.read_state().is_transient_item(loot_id));
        assert!(repo.read_state().items.contains_key(&loot_id));
        drop(repo);

        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen");
        assert!(!reopened.read_state().is_transient_item(transient_ship_id));
        assert!(reopened.read_state().items.contains_key(&loot_id));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(16))]
        #[test]
        fn transient_item_property_never_reaches_sqlite(
            raw_id in 200_500_000_i64..200_999_999_i64,
            qty in 1_i32..500_i32,
            sys_id in 30_000_001_u32..30_000_200_u32,
        ) {
            // parity: feat:serverops.persistence
            let dir = TestTempDir::new("prop_transient");
            let db_path = dir.path().join("prop_transient.sqlite");
            let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
            repo.tx("prop_insert_transient", |wtx| -> Result<(), TxError> {
                let mut item = make_test_item(raw_id, 100_001, qty);
                item.location_id = i64::from(sys_id);
                wtx.snap.insert_transient_item(item);
                Ok(())
            })
            .expect("prop_insert_transient");

            let cnt: i64 = repo
                .with_sqlite_conn(|conn| {
                    conn.query_row(
                        "SELECT COUNT(*) FROM items WHERE item_id = ?1",
                        params![raw_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())
                })
                .expect("count");
            prop_assert_eq!(cnt, 0);
            prop_assert_eq!(repo.read_state().wal_seq, 0);
            let encoded = serde_json::to_string(&*repo.read_state()).expect("json");
            prop_assert!(!encoded.contains(&raw_id.to_string()));
        }
    }

    #[test]
    fn id_allocator_cross_table_guard_prevents_duplicates_across_all_tables() {
        // parity: feat:serverops.persistence
        let dir = TestTempDir::new("id_cross_table");
        let db_path = dir.path().join("id_cross_table.sqlite");

        let seeded_ids: [i64; 22] = [
            200_000_101, // item
            200_000_102, // structure
            200_000_103, // mail
            200_000_104, // notification (owner_notes -79)
            200_000_105, // contract
            200_000_106, // bookmark
            200_000_107, // killmail
            200_000_108, // LP log
            200_000_109, // note (owner_notes)
            200_000_110, // market_order
            200_000_111, // market_transaction (domain_rows)
            200_000_112, // industry_job
            200_000_113, // fleet
            200_000_114, // war
            200_000_115, // corporation
            200_000_116, // alliance
            200_000_117, // saved_fitting
            200_000_118, // wallet_journal + wallet_journal_meta
            200_000_119, // item_positions
            200_000_120, // item_custom_names
            200_000_121, // applied_ship_skins
            200_000_122, // entity_kv
        ];
        let max_seeded = *seeded_ids.iter().max().expect("non-empty seeded_ids");

        {
            let repo = WorldRepository::open_sqlite(&db_path).expect("open_sqlite");
            repo.tx("seed_all_id_tables", |wtx| -> Result<(), TxError> {
                wtx.snap
                    .items
                    .insert(seeded_ids[0], make_test_item(seeded_ids[0], 90_000_001, 1));
                wtx.snap.structures.insert(
                    seeded_ids[1],
                    crate::types::UpwellStructureRecord {
                        structure_id: seeded_ids[1],
                        type_id: 35832,
                        owner_corp_id: 98_000_001,
                        solar_system_id: 30_000_142,
                        name: "Seeded Citadel".into(),
                        ..Default::default()
                    },
                );
                wtx.snap.mails.push(crate::types::MailRecord {
                    message_id: seeded_ids[2],
                    sender_id: 90_000_001,
                    recipient_id: 90_000_002,
                    subject: "Seed".into(),
                    body: "Body".into(),
                });
                let notif_key = -7_900_000_000_000_i64 - seeded_ids[3];
                wtx.snap.owner_notes.insert(
                    notif_key,
                    crate::types::OwnerNoteRecord {
                        note_id: notif_key,
                        character_id: -79,
                        item_id: 90_000_001,
                        label: "SeedNotif".into(),
                        note: "{}".into(),
                        timestamp_unix: 1,
                    },
                );
                wtx.snap.contracts.insert(
                    seeded_ids[4],
                    crate::types::ContractRecord {
                        contract_id: seeded_ids[4],
                        issuer_id: 90_000_001,
                        contract_type: "ItemExchange".into(),
                        status: "Outstanding".into(),
                        ..Default::default()
                    },
                );
                wtx.snap.bookmarks.insert(
                    seeded_ids[5],
                    crate::types::BookmarkRecord {
                        bookmark_id: seeded_ids[5],
                        owner_id: 90_000_001,
                        label: "BM".into(),
                        solar_system_id: 30_000_142,
                        ..Default::default()
                    },
                );
                wtx.snap.killmails.insert(
                    seeded_ids[6],
                    crate::types::KillmailRecord {
                        kill_id: seeded_ids[6],
                        victim_character_id: 90_000_001,
                        attacker_character_id: 90_000_002,
                        solar_system_id: 30_000_142,
                        ship_type_id: 587,
                        ..Default::default()
                    },
                );
                wtx.snap.char_lp_logs.insert(
                    90_000_001,
                    vec![crate::snapshot_ext::LpRewardLogEntry {
                        log_id: seeded_ids[7],
                        corp_id: 1_000_180,
                        amount: 100,
                        reason: "Seed".into(),
                        timestamp_unix: 1,
                    }],
                );
                wtx.snap.owner_notes.insert(
                    seeded_ids[8],
                    crate::types::OwnerNoteRecord {
                        note_id: seeded_ids[8],
                        character_id: 90_000_001,
                        item_id: 0,
                        label: "Note".into(),
                        note: "Body".into(),
                        timestamp_unix: 1,
                    },
                );
                wtx.snap.market_orders.insert(
                    seeded_ids[9],
                    crate::types::MarketOrderRecord {
                        order_id: seeded_ids[9],
                        owner_id: 90_000_001,
                        station_id: 60_003_760,
                        region_id: 10_000_002,
                        type_id: 34,
                        vol_entered: 10,
                        vol_remaining: 10,
                        min_volume: 1,
                        ..Default::default()
                    },
                );
                wtx.snap.market_transactions.insert(
                    seeded_ids[10],
                    crate::ledger::MarketTransactionRecord {
                        transaction_id: seeded_ids[10],
                        type_id: 34,
                        quantity: 1,
                        location_id: 60_003_760,
                        buyer_id: 90_000_001,
                        seller_id: 90_000_002,
                        ..Default::default()
                    },
                );
                wtx.snap.industry_jobs.insert(
                    seeded_ids[11],
                    crate::types::IndustryJobRecord {
                        job_id: seeded_ids[11],
                        installer_id: 90_000_001,
                        facility_id: 60_003_760,
                        activity_id: 1,
                        runs: 1,
                        status: "Active".into(),
                        ..Default::default()
                    },
                );
                wtx.snap.fleets.insert(
                    seeded_ids[12],
                    crate::types::FleetRecord {
                        fleet_id: seeded_ids[12],
                        boss_id: 90_000_001,
                        ..Default::default()
                    },
                );
                wtx.snap.wars.insert(
                    seeded_ids[13],
                    crate::types::WarRecord {
                        war_id: seeded_ids[13],
                        aggressor_corp_id: 98_000_001,
                        defender_corp_id: 98_000_002,
                        active: true,
                        ..Default::default()
                    },
                );
                wtx.snap.corporations.insert(
                    seeded_ids[14],
                    crate::types::CorporationRecord {
                        corporation_id: seeded_ids[14],
                        name: "SeedCorp1087".into(),
                        ticker: "S1087".into(),
                        ceo_id: 90_000_001,
                        ..Default::default()
                    },
                );
                wtx.snap.alliances.insert(
                    seeded_ids[15],
                    crate::types::AllianceRecord {
                        alliance_id: seeded_ids[15],
                        name: "SeedAlliance1087".into(),
                        short_name: "SA87".into(),
                        executor_corp_id: seeded_ids[14],
                        ..Default::default()
                    },
                );
                wtx.snap.saved_fittings.insert(
                    seeded_ids[16],
                    crate::types::SavedFittingRecord {
                        fitting_id: seeded_ids[16],
                        owner_id: 90_000_001,
                        name: "Fit1087".into(),
                        ship_type_id: 587,
                        ..Default::default()
                    },
                );
                wtx.snap.wallet_journal.push(crate::types::WalletJournalEntry {
                    entry_id: seeded_ids[17],
                    character_id: 90_000_001,
                    entry_type: "Test".into(),
                    ..Default::default()
                });
                wtx.snap
                    .wallet_journal_meta
                    .insert(seeded_ids[17], crate::ledger::JournalMeta::default());
                wtx.snap.item_positions.insert(seeded_ids[18], [1.0, 2.0, 3.0]);
                wtx.snap
                    .item_custom_names
                    .insert(seeded_ids[19], "CustomHull1087".into());
                wtx.snap.applied_ship_skins.insert(seeded_ids[20], 42);
                wtx.snap.entity_kv.insert(
                    crate::service_domains::EntityKvKey::new(
                        crate::service_domains::Ns::AlphaAvailNotified,
                        seeded_ids[21],
                        "",
                    ),
                    crate::service_domains::EntityKvRow {
                        value_json: "true".into(),
                        updated_unix: 1,
                    },
                );
                Ok(())
            })
            .expect("seed_all_id_tables");

            // Simulate stale persisted counter below all seeded IDs before restart.
            repo.with_sqlite_conn(|conn| {
                conn.execute(
                    "INSERT OR REPLACE INTO world_meta (meta_key, meta_value) VALUES ('next_entity_id', 200000000), ('next_entity_block', 200000000)",
                    [],
                )
                .map_err(|e| e.to_string())?;
                Ok::<(), String>(())
            })
            .expect("reset stale counter");
        }

        // Boot consults high-water mark over all ID-bearing tables and never duplicates seeded IDs.
        let reopened = WorldRepository::open_sqlite(&db_path).expect("reopen");
        assert!(
            reopened.read_state().next_entity_id >= max_seeded,
            "boot high-water mark must cover all ID-bearing tables, got {}",
            reopened.read_state().next_entity_id
        );

        let allocated = reopened
            .tx("alloc_post_boot", |wtx| -> Result<(i64, i64), TxError> {
                let a = wtx.alloc_entity_id()?;
                let b = wtx.snap.alloc_entity_id();
                Ok((a, b))
            })
            .expect("alloc_post_boot");
        assert!(allocated.0 > max_seeded);
        assert!(allocated.1 > allocated.0);
        for sid in seeded_ids {
            assert_ne!(allocated.0, sid);
            assert_ne!(allocated.1, sid);
        }

        // Verify transaction abort still advances persistent block/allocator so an aborted ID cannot collide.
        let aborted_id_cell = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
        let aborted_id_ref = std::sync::Arc::clone(&aborted_id_cell);
        let abort_res: Result<(), String> = reopened.tx("alloc_then_abort", |wtx| {
            let id = wtx
                .alloc_entity_id()
                .map_err(|e| format!("alloc failed: {e:?}"))?;
            aborted_id_ref.store(id, std::sync::atomic::Ordering::SeqCst);
            Err("intentional_abort".to_string())
        });
        assert_eq!(abort_res, Err("intentional_abort".to_string()));
        let aborted_id = aborted_id_cell.load(std::sync::atomic::Ordering::SeqCst);
        let post_abort_id = reopened
            .tx("alloc_after_abort", |wtx| wtx.alloc_entity_id())
            .expect("alloc_after_abort");
        assert!(
            post_abort_id > aborted_id,
            "post-abort allocation {post_abort_id} must remain strictly above aborted {aborted_id}"
        );

        // Also prove candidate skipping works when tables hold IDs >= 900_000_000 right ahead of counter.
        reopened
            .tx("skip_900m_collision", |wtx| -> Result<(), TxError> {
                let high_struct_id = 900_000_501_i64;
                let high_tx_id = 900_000_502_i64;
                let high_pos_id = 900_000_503_i64;
                wtx.snap.structures.insert(
                    high_struct_id,
                    crate::types::UpwellStructureRecord {
                        structure_id: high_struct_id,
                        type_id: 35832,
                        owner_corp_id: 98_000_001,
                        solar_system_id: 30_000_142,
                        name: "High Collision Citadel".into(),
                        ..Default::default()
                    },
                );
                wtx.snap.market_transactions.insert(
                    high_tx_id,
                    crate::ledger::MarketTransactionRecord {
                        transaction_id: high_tx_id,
                        type_id: 34,
                        quantity: 1,
                        ..Default::default()
                    },
                );
                wtx.snap
                    .item_positions
                    .insert(high_pos_id, [10.0, 20.0, 30.0]);
                wtx.snap.next_entity_id = high_struct_id - 1;
                let picked = wtx.alloc_entity_id()?;
                assert_ne!(picked, high_struct_id);
                assert_ne!(picked, high_tx_id);
                assert_ne!(picked, high_pos_id);
                assert_eq!(picked, high_pos_id + 1);
                Ok(())
            })
            .expect("skip_900m_collision");

        // Account/character boundaries (player + GM) commit counter synchronously to world_meta.
        let uid = reopened.provision_player_account("sync_counter_user", "pw");
        assert!(uid > 0);
        reopened.provision_gm_account("gm_sync_1087", "gm_pw", 90_000_199, "ISD Sync");
        let (meta_id, meta_block): (i64, i64) = reopened
            .with_sqlite_conn(|conn| {
                let id: i64 = conn
                    .query_row(
                        "SELECT meta_value FROM world_meta WHERE meta_key = 'next_entity_id'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                let block: i64 = conn
                    .query_row(
                        "SELECT meta_value FROM world_meta WHERE meta_key = 'next_entity_block'",
                        [],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                Ok::<(i64, i64), String>((id, block))
            })
            .expect("read world_meta");
        assert!(meta_id >= reopened.read_state().next_entity_id);
        assert!(meta_block >= meta_id);
    }

    #[test]
    fn entity_id_allocator_keeps_bounded_memory_across_one_million_sequential_allocations() {
        let conn = Connection::open_in_memory().expect("open_in_memory");
        crate::sqlite_store::apply_migrations(&conn).expect("apply_migrations");
        let alloc = EntityIdAllocator::new(200_000_000, 200_000_000);
        let snap = WorldStateSnapshot::default();

        let mut last = 200_000_000_i64;
        for _ in 0..1_000_000 {
            let id = alloc
                .alloc_with_conn_and_snap(&conn, &snap)
                .expect("sequential alloc_with_conn_and_snap");
            assert_eq!(id, last + 1);
            last = id;
        }
        assert_eq!(alloc.current(), 201_000_000);
        assert_eq!(
            alloc.referenced_ids.lock().unwrap().len(),
            0,
            "sequential allocations must not grow referenced_ids in memory"
        );

        // Stale / covered IDs at or below watermark must not be retained.
        alloc.mark_referenced(200_000_500);
        alloc.mark_referenced(201_000_000);
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 0);
        assert_eq!(alloc.current(), 201_000_000);

        // Contiguous ID right above watermark advances `next` directly with O(1) set storage.
        alloc.mark_referenced(201_000_001);
        assert_eq!(alloc.current(), 201_000_001);
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 0);

        // Out-of-order ID strictly above watermark is retained until allocated past, then pruned.
        alloc.mark_referenced(201_000_005);
        assert!(alloc.is_referenced(201_000_005));
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 1);
        assert_eq!(alloc.alloc_with_conn_and_snap(&conn, &snap).unwrap(), 201_000_002);
        assert_eq!(alloc.alloc_with_conn_and_snap(&conn, &snap).unwrap(), 201_000_003);
        assert_eq!(alloc.alloc_with_conn_and_snap(&conn, &snap).unwrap(), 201_000_004);
        assert_eq!(alloc.alloc_with_conn_and_snap(&conn, &snap).unwrap(), 201_000_006);
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 0);

        // Bridging a gap via contiguous mark_referenced collapses retained entries into `next`.
        alloc.mark_referenced(201_000_008);
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 1);
        alloc.mark_referenced(201_000_007);
        assert_eq!(alloc.current(), 201_000_008);
        assert_eq!(alloc.referenced_ids.lock().unwrap().len(), 0);
    }
}
