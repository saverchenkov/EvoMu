//! Transactional client delta projection from committed `eve_db::ChangeSet` values (`#931`, `#862`).
//!
//! Derives V24.01 Macho notifications (`OnItemChange`, `OnBlueprintsUpdated`, `OnAccountChange`,
//! `OnServerSkillsChanged`, `OnServerSkillsRemoved`, `OnNewSkillQueueSaved`,
//! `OnFreeSkillPointsChanged`, `OnSkillQueuePausedServer`, `OnOwnOrdersChanged`, and Godma
//! `OnModuleAttributeChanges` fit diffs) solely from committed `ChangeSet` deltas + world state,
//! ensuring client caches stay converged with authoritative transactions without hand-written emits.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::{
    is_corp_hangar_flag, NotificationTarget, PendingMachoNotification,
};
use crate::router::notify_registry::{
    build_pending_registered_notification, OnAccountChange, OnBlueprintsUpdated,
    OnFreeSkillPointsChanged, OnItemChange, OnModuleAttributeChanges, OnNewSkillQueueSaved,
    OnOwnOrdersChanged, OnServerSkillsChanged, OnServerSkillsRemoved, OnSkillQueuePausedServer,
    TypedNotification,
};
use eve_db::{
    ChangeSet, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord, SkillQueueEntry,
    TrainedSkillRecord, TypeId, WorldRepository, WorldStateSnapshot,
};
use eve_dogma::ShipFittingEngine;
use eve_proto::{DBRowDescriptor, PyRep};
use eve_sde::SdeRegistry;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::Mutex;

const FILETIME_UNIX_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;
const LOC_JUNKYARD: i64 = 6;
const CATEGORY_BLUEPRINT: u32 = 9;
const NPC_CORP_RANGE_START: i64 = 1_000_000;
const NPC_CORP_RANGE_END: i64 = 2_000_000;

/// Logical routing scope for a projected Macho notification before resolving online characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProjectedTarget {
    Character(i64),
    Corporation(i64),
}

impl ProjectedTarget {
    /// Converts into [`NotificationTarget`] for existing router delivery paths.
    pub fn to_notification_target(self) -> NotificationTarget {
        match self {
            Self::Character(cid) => NotificationTarget::Character(cid),
            Self::Corporation(corp_id) => NotificationTarget::Corporation(corp_id),
        }
    }

    /// Canonical Macho `id_type` string for this target scope.
    pub fn default_id_type(self) -> &'static str {
        match self {
            Self::Character(_) => "charid",
            Self::Corporation(_) => "corpid",
        }
    }
}

/// A projected V24.01 notification ready for recipient resolution and Macho packet encoding.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedNotification {
    pub target: ProjectedTarget,
    pub event: &'static str,
    pub id_type: &'static str,
    pub args: Vec<PyRep>,
}

impl ProjectedNotification {
    /// Builds a [`ProjectedNotification`] from a strongly typed [`TypedNotification`] using its
    /// registered `EVENT` and `DEFAULT_ID_TYPE`.
    pub fn from_typed<N: TypedNotification>(target: ProjectedTarget, notif: N) -> Self {
        Self {
            target,
            event: N::EVENT,
            id_type: N::DEFAULT_ID_TYPE,
            args: notif.into_args(),
        }
    }

    /// Builds a [`ProjectedNotification`] from a strongly typed [`TypedNotification`] with an
    /// explicit Macho `id_type` (for scope-dependent events like `OnItemChange` or `OnAccountChange`).
    pub fn from_typed_with_id_type<N: TypedNotification>(
        target: ProjectedTarget,
        id_type: &'static str,
        notif: N,
    ) -> Self {
        Self {
            target,
            event: N::EVENT,
            id_type,
            args: notif.into_args(),
        }
    }
}

/// Recipient-resolved Macho notification targeting a specific online `(char_id, user_id)`.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNotification {
    pub recipient_char_id: i64,
    pub recipient_user_id: Option<u64>,
    pub event: &'static str,
    pub id_type: &'static str,
    pub args: Vec<PyRep>,
}

/// Collects projected notifications emitted during a `ChangeSet` projection pass.
pub trait DeltaSink {
    fn push(&mut self, notification: ProjectedNotification);
}

impl DeltaSink for Vec<ProjectedNotification> {
    fn push(&mut self, notification: ProjectedNotification) {
        Vec::push(self, notification);
    }
}

/// Abstraction over active session lookup so both production `WorldRepository` and pure unit tests
/// can resolve online characters deterministically.
pub trait OnlineCharacterLookup {
    fn is_character_online(&self, char_id: i64, user_id: u64) -> bool;
}

impl OnlineCharacterLookup for WorldRepository {
    fn is_character_online(&self, char_id: i64, user_id: u64) -> bool {
        self.get_online_session(user_id)
            .is_some_and(|os| os.character_id == Some(char_id))
    }
}

impl OnlineCharacterLookup for BTreeSet<i64> {
    fn is_character_online(&self, char_id: i64, _user_id: u64) -> bool {
        self.contains(&char_id)
    }
}

impl OnlineCharacterLookup for HashSet<i64> {
    fn is_character_online(&self, char_id: i64, _user_id: u64) -> bool {
        self.contains(&char_id)
    }
}

/// Account PLEX / vault balance delta observed during projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlexDelta {
    pub char_id: Option<i64>,
    pub user_id: u64,
    pub old_plex: i64,
    pub new_plex: i64,
}

/// Fit-projection attribute diff input passed to [`DefaultGodmaFitHook`].
#[derive(Debug, Clone, PartialEq)]
pub struct FitProjectionDiff {
    pub char_id: i64,
    pub ship_id: i64,
    pub before: FitProjectionSnapshot,
    pub after: FitProjectionSnapshot,
}

/// Pre- and post-mutation evaluated attributes for a ship hull and its fitted modules.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FitProjectionSnapshot {
    pub ship_attributes: BTreeMap<u16, f64>,
    pub module_attributes: BTreeMap<i64, BTreeMap<u16, f64>>,
}

impl FitProjectionSnapshot {
    /// Captures evaluated ship and fitted module attributes from a [`ShipFittingEngine`].
    pub fn from_fitting_engine(fit: &ShipFittingEngine, sde: &SdeRegistry) -> Self {
        let eval_ctx = fit.evaluated_context(sde);
        let mut ship_attr_ids: BTreeSet<u16> = eval_ctx
            .ship
            .base_attributes
            .keys()
            .copied()
            .chain(
                eval_ctx
                    .ship
                    .modifiers
                    .iter()
                    .map(|m| m.target_attribute_id),
            )
            .collect();
        ship_attr_ids.insert(eve_sde::attr_ids::CPU_LOAD);
        ship_attr_ids.insert(eve_sde::attr_ids::POWER_LOAD);
        let ship_attributes = ship_attr_ids
            .into_iter()
            .map(|attr_id| (attr_id, eval_ctx.ship.get_attribute(attr_id, sde)))
            .collect();

        let mut module_attributes = BTreeMap::new();
        for (&module_id, fitted) in &fit.modules {
            let mut mod_attr_ids: BTreeSet<u16> = BTreeSet::new();
            if let Some(eval_mod) = fit.evaluated_modules.get(&module_id) {
                mod_attr_ids.extend(eval_mod.base_attributes.keys().copied());
                mod_attr_ids.extend(eval_mod.modifiers.iter().map(|m| m.target_attribute_id));
            } else if let Some(inv_t) = sde.types.get(&fitted.type_id) {
                mod_attr_ids.extend(inv_t.default_attributes.keys().copied());
            }
            mod_attr_ids.extend(fitted.dynamic_attribute_overrides.keys().copied());
            // Always track dogma online attribute (attr 2) alongside evaluated module stats.
            mod_attr_ids.insert(2);
            let attrs = mod_attr_ids
                .into_iter()
                .map(|attr_id| {
                    (
                        attr_id,
                        fit.get_module_effective_attribute(module_id, attr_id, sde),
                    )
                })
                .collect();
            module_attributes.insert(module_id, attrs);
        }

        Self {
            ship_attributes,
            module_attributes,
        }
    }
}

/// Extensible hook interface for projecting additional domain deltas (such as Godma fit diffs or PLEX events).
pub trait DeltaProjectionHook: Send + Sync {
    /// Invoked after standard `ChangeSet` projection to emit custom domain notifications.
    fn on_project(&self, input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
        let _ = (input, sink);
    }

    /// Invoked when character `plex_balance` or `account_plex_vault` balances change.
    fn on_plex_delta(&self, delta: PlexDelta, sink: &mut dyn DeltaSink) {
        let _ = (delta, sink);
    }
}

/// Default Godma fit-projection hook converting [`FitProjectionDiff`]s into `OnModuleAttributeChanges`.
#[derive(Debug, Default)]
pub struct DefaultGodmaFitHook;

impl DeltaProjectionHook for DefaultGodmaFitHook {
    fn on_project(&self, input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
        for diff in input.fit_diffs {
            if let Some(notif) = project_fit_attribute_diff(
                diff.char_id,
                diff.ship_id,
                &diff.before,
                &diff.after,
                input.now_filetime(),
            ) {
                sink.push(notif);
            }
        }
    }
}

/// Borrowed inputs for projecting one committed [`ChangeSet`].
pub struct DeltaProjectionInput<'a> {
    pub before: Option<&'a WorldStateSnapshot>,
    pub after: &'a WorldStateSnapshot,
    pub changes: &'a ChangeSet,
    pub sde: &'a SdeRegistry,
    pub now_unix: u64,
    pub fit_diffs: &'a [FitProjectionDiff],
    /// Optional explicit pre-commit corp wallet baseline when `before` snapshot is unavailable.
    pub corp_wallet_before: Option<&'a HashMap<(i64, u8), IskCents>>,
    /// Optional explicit pre-commit account PLEX vault baseline when `before` snapshot is unavailable.
    pub plex_vault_before: Option<&'a HashMap<u64, i64>>,
}

impl<'a> DeltaProjectionInput<'a> {
    pub fn new(
        before: Option<&'a WorldStateSnapshot>,
        after: &'a WorldStateSnapshot,
        changes: &'a ChangeSet,
        sde: &'a SdeRegistry,
        now_unix: u64,
    ) -> Self {
        Self {
            before,
            after,
            changes,
            sde,
            now_unix,
            fit_diffs: &[],
            corp_wallet_before: None,
            plex_vault_before: None,
        }
    }

    pub fn with_fit_diffs(mut self, fit_diffs: &'a [FitProjectionDiff]) -> Self {
        self.fit_diffs = fit_diffs;
        self
    }

    pub fn with_corp_wallet_before(
        mut self,
        corp_wallet_before: &'a HashMap<(i64, u8), IskCents>,
    ) -> Self {
        self.corp_wallet_before = Some(corp_wallet_before);
        self
    }

    pub fn with_plex_vault_before(mut self, plex_vault_before: &'a HashMap<u64, i64>) -> Self {
        self.plex_vault_before = Some(plex_vault_before);
        self
    }

    pub fn now_filetime(&self) -> i64 {
        unix_to_filetime(self.now_unix)
    }
}

/// Core projection trait converting a committed `ChangeSet` into V24.01 client deltas.
pub trait DeltaProjection {
    fn project(&self, input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink);
}

/// Standard V24.01 delta projector covering inventory items, blueprints, wallets (personal ISK
/// and corp divisions 1..7), skills/queues, market orders, and registered fit/custom hooks.
pub struct DefaultDeltaProjection {
    hooks: Vec<Box<dyn DeltaProjectionHook>>,
}

impl Default for DefaultDeltaProjection {
    fn default() -> Self {
        Self::new()
    }
}

impl DefaultDeltaProjection {
    /// Creates a projector pre-configured with [`DefaultGodmaFitHook`].
    pub fn new() -> Self {
        Self {
            hooks: vec![Box::new(DefaultGodmaFitHook)],
        }
    }

    /// Creates a projector with zero extra hooks (core `ChangeSet` projection only).
    pub fn without_hooks() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Registers an additional domain projection hook.
    pub fn add_hook(&mut self, hook: Box<dyn DeltaProjectionHook>) {
        self.hooks.push(hook);
    }

    /// Convenience helper returning all projected notifications as a `Vec`.
    pub fn project_to_vec(&self, input: &DeltaProjectionInput<'_>) -> Vec<ProjectedNotification> {
        let mut out = Vec::new();
        self.project(input, &mut out);
        out
    }

    /// Projects and resolves online `(char_id, user_id)` recipients for delivery.
    pub fn project_and_resolve(
        &self,
        input: &DeltaProjectionInput<'_>,
        online: &impl OnlineCharacterLookup,
    ) -> Vec<ResolvedNotification> {
        let projected = self.project_to_vec(input);
        resolve_all_notifications(projected, input.after, input.before, online)
    }
}

impl DeltaProjection for DefaultDeltaProjection {
    fn project(&self, input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
        project_item_changes(input, sink);
        project_wallet_changes(input, &self.hooks, sink);
        project_skill_changes(input, sink);
        project_market_order_changes(input, sink);
        for hook in &self.hooks {
            hook.on_project(input, sink);
        }
    }
}

/// Stateful cache used by post-commit listeners when callers only provide `(&after, &changes)`
/// without retaining the full `before` [`WorldStateSnapshot`] across the commit boundary.
#[derive(Debug, Default)]
pub struct DeltaStateCache {
    corp_wallets: Mutex<HashMap<(i64, u8), IskCents>>,
    plex_vault: Mutex<HashMap<u64, i64>>,
    paused_skill_queues: Mutex<HashSet<i64>>,
    seeded: Mutex<bool>,
}

impl DeltaStateCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seeds initial baselines from a snapshot (typically once at listener registration).
    pub fn seed_from_snapshot(&self, snap: &WorldStateSnapshot) {
        let mut corp_guard = self.corp_wallets.lock().unwrap_or_else(|e| e.into_inner());
        corp_guard.clear();
        for (&corp_id, corp) in &snap.corporations {
            for div_id in 1..=7u8 {
                corp_guard.insert((corp_id, div_id), corp.division_balance(div_id));
            }
        }
        let mut plex_guard = self.plex_vault.lock().unwrap_or_else(|e| e.into_inner());
        plex_guard.clear();
        for (&user_id, &plex) in snap.account_plex_vault.iter() {
            plex_guard.insert(user_id, plex);
        }
        let mut paused_guard = self
            .paused_skill_queues
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *paused_guard = snap.paused_skill_queues.clone();
        *self.seeded.lock().unwrap_or_else(|e| e.into_inner()) = true;
    }

    /// Projects a post-commit `(after, changes)` pair using cached cold-domain baselines when
    /// `before` is `None`, then advances cached baselines to `after`.
    pub fn project_with_cache(
        &self,
        projector: &DefaultDeltaProjection,
        before: Option<&WorldStateSnapshot>,
        after: &WorldStateSnapshot,
        changes: &ChangeSet,
        sde: &SdeRegistry,
        now_unix: u64,
        fit_diffs: &[FitProjectionDiff],
    ) -> Vec<ProjectedNotification> {
        let is_seeded = *self.seeded.lock().unwrap_or_else(|e| e.into_inner());
        if !is_seeded && before.is_none() {
            // Without an initial baseline, initialize cache from `after` only if no cold corp diff occurred;
            // callers should invoke `seed_from_snapshot` during startup.
            self.seed_from_snapshot(after);
        }

        let corp_baseline_snapshot = if before.is_none() && changes.cold_domains.part3_social_corp {
            Some(
                self.corp_wallets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
        } else {
            None
        };
        let plex_vault_changed = changes_touch_account_plex_vault(changes);
        let plex_baseline_snapshot = if before.is_none() && plex_vault_changed {
            Some(
                self.plex_vault
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
        } else {
            None
        };

        let mut input = DeltaProjectionInput::new(before, after, changes, sde, now_unix)
            .with_fit_diffs(fit_diffs);
        if let Some(ref map) = corp_baseline_snapshot {
            input = input.with_corp_wallet_before(map);
        }
        if let Some(ref map) = plex_baseline_snapshot {
            input = input.with_plex_vault_before(map);
        }

        let mut out = projector.project_to_vec(&input);

        if before.is_none() && changes.cold_domains.d6_batch4 {
            let mut paused_guard = self
                .paused_skill_queues
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let mut newly_paused: Vec<i64> = after
                .paused_skill_queues
                .iter()
                .filter(|cid| !paused_guard.contains(cid))
                .copied()
                .collect();
            newly_paused.sort_unstable();
            for char_id in newly_paused {
                out.push(ProjectedNotification::from_typed(
                    ProjectedTarget::Character(char_id),
                    OnSkillQueuePausedServer { paused: None },
                ));
            }
            *paused_guard = after.paused_skill_queues.clone();
        }

        if changes.cold_domains.part3_social_corp {
            let mut corp_guard = self.corp_wallets.lock().unwrap_or_else(|e| e.into_inner());
            for (&corp_id, corp) in &after.corporations {
                for div_id in 1..=7u8 {
                    corp_guard.insert((corp_id, div_id), corp.division_balance(div_id));
                }
            }
        }
        if plex_vault_changed {
            let mut plex_guard = self.plex_vault.lock().unwrap_or_else(|e| e.into_inner());
            plex_guard.clear();
            for (&user_id, &plex) in after.account_plex_vault.iter() {
                plex_guard.insert(user_id, plex);
            }
        }
        if before.is_some() && changes.cold_domains.d6_batch4 {
            let mut paused_guard = self
                .paused_skill_queues
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            *paused_guard = after.paused_skill_queues.clone();
        }

        out
    }
}

/// Converts Unix seconds to Windows FILETIME ticks expected by EVE V24.01 notifications.
pub fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + FILETIME_UNIX_EPOCH_OFFSET_SECS).saturating_mul(FILETIME_TICKS_PER_SEC)
}

/// Returns the V24 `OnAccountChange` key string (`"cash"` for division 1, `"cash2".."cash7"` for 2..=7).
pub fn corp_wallet_account_key(division_id: u8) -> &'static str {
    match division_id {
        0 | 1 => "cash",
        2 => "cash2",
        3 => "cash3",
        4 => "cash4",
        5 => "cash5",
        6 => "cash6",
        7 => "cash7",
        _ => "cash",
    }
}

/// Builds the 9-column packed `utillib.DBRow` cells for [`DBRowDescriptor::inv_item_header`].
pub fn build_inv_item_cells(
    item: &InventoryItemRecord,
    sde: &SdeRegistry,
    custom_name: &str,
) -> Vec<PyRep> {
    let (group_id, category_id) = sde
        .types
        .get(&item.type_id)
        .map(|t| (t.group_id as i64, t.category_id as i64))
        .unwrap_or((0, 0));
    let wire_qty = if item.is_blueprint_copy {
        -2
    } else if item.singleton {
        -1
    } else {
        item.quantity as i64
    };
    vec![
        PyRep::Integer(item.item_id),
        PyRep::Integer(item.type_id as i64),
        PyRep::Integer(item.owner_id),
        PyRep::Integer(item.location_id),
        PyRep::Integer(item.flag as i64),
        PyRep::Integer(wire_qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(custom_name.to_string()),
    ]
}

/// Builds a canonical `inv_item_header` `DBRow` for an inventory item.
pub fn build_inv_item_dbrow(
    item: &InventoryItemRecord,
    sde: &SdeRegistry,
    custom_name: &str,
) -> PyRep {
    DBRowDescriptor::inv_item_header().build_dbrow(build_inv_item_cells(item, sde, custom_name))
}

/// Computes canonical `OnItemChange` delta entries (`col_index -> old_value`).
///
/// Uses `ixStackSize` (`9`) for stack quantity deltas and never emits `ixQuantity` (`5`),
/// matching client `InvCache` expectations (`clientDogmaLocation`, `eveCfg.StackSize`).
pub fn compute_item_change_entries(
    old: &InventoryItemRecord,
    new: &InventoryItemRecord,
) -> Vec<(i32, PyRep)> {
    let mut changes = Vec::new();
    if old.owner_id != new.owner_id {
        changes.push((2, PyRep::Integer(old.owner_id)));
    }
    if old.location_id != new.location_id {
        changes.push((3, PyRep::Integer(old.location_id)));
    }
    if old.flag != new.flag {
        changes.push((4, PyRep::Integer(old.flag as i64)));
    }
    if old.quantity != new.quantity {
        changes.push((9, PyRep::Integer(old.quantity as i64)));
    }
    if old.singleton != new.singleton {
        changes.push((10, PyRep::Integer(if old.singleton { 1 } else { 0 })));
    }
    changes
}

/// Builds a `locJunkyard` (`locationID = 6`) tombstone record and old-value change dict for deleted items.
pub fn build_item_tombstone(old: &InventoryItemRecord) -> (InventoryItemRecord, Vec<(i32, PyRep)>) {
    let mut tomb = old.clone();
    tomb.location_id = LOC_JUNKYARD;
    tomb.flag = InventoryFlag::None;
    tomb.quantity = 0;
    tomb.singleton = false;
    tomb.is_blueprint_copy = false;
    let changes = vec![
        (3, PyRep::Integer(old.location_id)),
        (4, PyRep::Integer(old.flag as i64)),
        (9, PyRep::Integer(old.quantity as i64)),
    ];
    (tomb, changes)
}

fn build_on_item_change(
    item: &InventoryItemRecord,
    changes: Vec<(i32, PyRep)>,
    sde: &SdeRegistry,
    custom_name: &str,
) -> OnItemChange {
    let row = build_inv_item_dbrow(item, sde, custom_name);
    let change_dict = PyRep::Dict(
        changes
            .into_iter()
            .map(|(col_ix, old_val)| (PyRep::Integer(col_ix as i64), old_val))
            .collect(),
    );
    OnItemChange {
        item: row,
        change: change_dict,
        location: PyRep::None,
    }
}

fn is_blueprint_type(sde: &SdeRegistry, type_id: TypeId) -> bool {
    sde.blueprints.contains_key(&type_id)
        || sde
            .types
            .get(&type_id)
            .is_some_and(|t| t.category_id == CATEGORY_BLUEPRINT)
}

fn is_known_corporation(
    corp_id: i64,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
) -> bool {
    after.corporations.contains_key(&corp_id)
        || before.is_some_and(|b| b.corporations.contains_key(&corp_id))
}

fn is_known_character(
    char_id: i64,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
) -> bool {
    after.characters.contains_key(&char_id)
        || before.is_some_and(|b| b.characters.contains_key(&char_id))
}

/// Resolves whether an inventory item mutation routes to a corporation scope or character scope.
fn resolve_item_target(
    item: &InventoryItemRecord,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
) -> Option<ProjectedTarget> {
    if item.owner_id <= 0 {
        return None;
    }
    if is_corp_hangar_flag(item.flag) && is_known_corporation(item.owner_id, after, before) {
        return Some(ProjectedTarget::Corporation(item.owner_id));
    }
    if is_known_character(item.owner_id, after, before) {
        return Some(ProjectedTarget::Character(item.owner_id));
    }
    if is_known_corporation(item.owner_id, after, before) {
        return Some(ProjectedTarget::Corporation(item.owner_id));
    }
    None
}

fn resolve_blueprint_target(
    owner_id: i64,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
) -> Option<ProjectedTarget> {
    if owner_id <= 0 {
        return None;
    }
    let is_corp = is_known_corporation(owner_id, after, before)
        || (!is_known_character(owner_id, after, before)
            && (NPC_CORP_RANGE_START..=NPC_CORP_RANGE_END).contains(&owner_id));
    if is_corp {
        Some(ProjectedTarget::Corporation(owner_id))
    } else if is_known_character(owner_id, after, before) {
        Some(ProjectedTarget::Character(owner_id))
    } else {
        None
    }
}

fn push_blueprint_update_dedup(
    owner_id: i64,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
    emitted_bp: &mut BTreeSet<(ProjectedTarget, i64)>,
    sink: &mut dyn DeltaSink,
) {
    let Some(target) = resolve_blueprint_target(owner_id, after, before) else {
        return;
    };
    if emitted_bp.insert((target, owner_id)) {
        sink.push(ProjectedNotification::from_typed(
            target,
            OnBlueprintsUpdated { owner_id },
        ));
    }
}

fn project_item_changes(input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
    if input.changes.items.is_empty() {
        return;
    }
    let mut emitted_bp = BTreeSet::new();

    for change in &input.changes.items {
        let key = &change.key;
        match (change.before.as_deref(), change.after.as_deref()) {
            (None, Some(item)) => {
                let custom_name = input
                    .after
                    .item_custom_names
                    .get(key)
                    .map(String::as_str)
                    .unwrap_or("");
                if let Some(target) = resolve_item_target(item, input.after, input.before) {
                    // Signal prior locationID = 0 (`ixLocationID` = 3) so V24 `invCache` / open `InvCont`
                    // windows insert newly created items immediately instead of only updating capacity (#1026).
                    sink.push(ProjectedNotification::from_typed_with_id_type(
                        target,
                        target.default_id_type(),
                        build_on_item_change(
                            item,
                            vec![(3, PyRep::Integer(0))],
                            input.sde,
                            custom_name,
                        ),
                    ));
                }
                if is_blueprint_type(input.sde, item.type_id) {
                    push_blueprint_update_dedup(
                        item.owner_id,
                        input.after,
                        input.before,
                        &mut emitted_bp,
                        sink,
                    );
                }
            }
            (Some(old), None) => {
                let custom_name = input
                    .before
                    .and_then(|b| b.item_custom_names.get(key))
                    .map(String::as_str)
                    .unwrap_or("");
                let (tomb, tomb_changes) = build_item_tombstone(old);
                if let Some(target) = resolve_item_target(old, input.after, input.before) {
                    sink.push(ProjectedNotification::from_typed_with_id_type(
                        target,
                        target.default_id_type(),
                        build_on_item_change(&tomb, tomb_changes, input.sde, custom_name),
                    ));
                }
                if is_blueprint_type(input.sde, old.type_id) {
                    push_blueprint_update_dedup(
                        old.owner_id,
                        input.after,
                        input.before,
                        &mut emitted_bp,
                        sink,
                    );
                }
            }
            (Some(old), Some(new)) => {
                let item_changes = compute_item_change_entries(old, new);
                let bp_involved = is_blueprint_type(input.sde, new.type_id)
                    || is_blueprint_type(input.sde, old.type_id);
                let bp_metadata_changed = bp_involved
                    && (old.type_id != new.type_id
                        || old.is_blueprint_copy != new.is_blueprint_copy
                        || old.blueprint_runs_remaining != new.blueprint_runs_remaining
                        || old.dynamic_attributes != new.dynamic_attributes);

                if item_changes.is_empty() && !bp_metadata_changed {
                    continue;
                }

                if !item_changes.is_empty() {
                    let custom_name = input
                        .after
                        .item_custom_names
                        .get(key)
                        .map(String::as_str)
                        .unwrap_or("");
                    let notif = build_on_item_change(new, item_changes, input.sde, custom_name);
                    let new_target = resolve_item_target(new, input.after, input.before);
                    let old_target = resolve_item_target(old, input.after, input.before);

                    if let Some(nt) = new_target {
                        let ot_differs = old_target.is_some_and(|ot| ot != nt);
                        if ot_differs {
                            sink.push(ProjectedNotification::from_typed_with_id_type(
                                nt,
                                nt.default_id_type(),
                                OnItemChange {
                                    item: notif.item.clone(),
                                    change: notif.change.clone(),
                                    location: notif.location.clone(),
                                },
                            ));
                            if let Some(ot) = old_target {
                                sink.push(ProjectedNotification::from_typed_with_id_type(
                                    ot,
                                    ot.default_id_type(),
                                    notif,
                                ));
                            }
                        } else {
                            sink.push(ProjectedNotification::from_typed_with_id_type(
                                nt,
                                nt.default_id_type(),
                                notif,
                            ));
                        }
                    } else if let Some(ot) = old_target {
                        sink.push(ProjectedNotification::from_typed_with_id_type(
                            ot,
                            ot.default_id_type(),
                            notif,
                        ));
                    }
                }

                if bp_involved {
                    push_blueprint_update_dedup(
                        new.owner_id,
                        input.after,
                        input.before,
                        &mut emitted_bp,
                        sink,
                    );
                    if old.owner_id != new.owner_id {
                        push_blueprint_update_dedup(
                            old.owner_id,
                            input.after,
                            input.before,
                            &mut emitted_bp,
                            sink,
                        );
                    }
                }
            }
            (None, None) => {}
        }
    }
}

fn project_wallet_changes(
    input: &DeltaProjectionInput<'_>,
    hooks: &[Box<dyn DeltaProjectionHook>],
    sink: &mut dyn DeltaSink,
) {
    // 1. Personal character ISK wallets & character-level PLEX hook.
    for change in &input.changes.characters {
        if let (Some(before), Some(after)) = (change.before.as_deref(), change.after.as_deref()) {
            let char_id = change.key;
            if before.balance != after.balance {
                sink.push(ProjectedNotification::from_typed(
                    ProjectedTarget::Character(char_id),
                    OnAccountChange {
                        account_key: "cash".into(),
                        owner_id: char_id,
                        balance: after.balance.to_wire_f64(),
                    },
                ));
            }
            if before.plex_balance != after.plex_balance {
                let delta = PlexDelta {
                    char_id: Some(char_id),
                    user_id: after.user_id,
                    old_plex: before.plex_balance,
                    new_plex: after.plex_balance,
                };
                for hook in hooks {
                    hook.on_plex_delta(delta, sink);
                }
            }
        }
    }

    // 2. Corporation wallet divisions 1..=7 (`"cash"`, `"cash2".."cash7"`).
    // `corporations` lives in `cold_domains.part3_social_corp`, so we skip in O(1) when false.
    if input.changes.cold_domains.part3_social_corp {
        let mut corp_ids: Vec<i64> = input.after.corporations.keys().copied().collect();
        corp_ids.sort_unstable();
        for corp_id in corp_ids {
            let after_corp = &input.after.corporations[&corp_id];
            for div_id in 1..=7u8 {
                let new_bal = after_corp.division_balance(div_id);
                let old_bal_opt = input
                    .before
                    .and_then(|b| b.corporations.get(&corp_id))
                    .map(|c| c.division_balance(div_id))
                    .or_else(|| {
                        input
                            .corp_wallet_before
                            .and_then(|m| m.get(&(corp_id, div_id)).copied())
                    });
                let Some(old_bal) = old_bal_opt else {
                    continue;
                };
                if old_bal != new_bal {
                    sink.push(ProjectedNotification::from_typed_with_id_type(
                        ProjectedTarget::Corporation(corp_id),
                        "corpid",
                        OnAccountChange {
                            account_key: corp_wallet_account_key(div_id).into(),
                            owner_id: corp_id,
                            balance: new_bal.to_wire_f64(),
                        },
                    ));
                }
            }
        }
    }

    // 3. Account-level PLEX vault ledger hook (`domain_rows` domain `ledger.account_plex_vault`).
    let plex_user_ids = changed_account_plex_vault_users(input.changes);
    if !plex_user_ids.is_empty() && (input.before.is_some() || input.plex_vault_before.is_some()) {
        for user_id in plex_user_ids {
            let old_plex = input
                .before
                .and_then(|b| b.account_plex_vault.get(&user_id).copied())
                .or_else(|| {
                    input
                        .plex_vault_before
                        .and_then(|m| m.get(&user_id).copied())
                })
                .unwrap_or(0);
            let new_plex = input
                .after
                .account_plex_vault
                .get(&user_id)
                .copied()
                .unwrap_or(0);
            if old_plex != new_plex {
                let delta = PlexDelta {
                    char_id: None,
                    user_id,
                    old_plex,
                    new_plex,
                };
                for hook in hooks {
                    hook.on_plex_delta(delta, sink);
                }
            }
        }
    }
}

fn changed_account_plex_vault_users(changes: &ChangeSet) -> BTreeSet<u64> {
    let mut user_ids = BTreeSet::new();
    for row in &changes.domain_rows {
        let (domain, key) = match row {
            eve_db::sqlite_store::DomainRowChange::Upsert { domain, key, .. } => {
                (*domain, key.as_str())
            }
            eve_db::sqlite_store::DomainRowChange::Delete { domain, key } => {
                (*domain, key.as_str())
            }
        };
        if domain == eve_db::sqlite_store::DOMAIN_ACCOUNT_PLEX_VAULT {
            if let Ok(uid) = key.parse::<u64>() {
                user_ids.insert(uid);
            }
        }
    }
    user_ids
}

fn changes_touch_account_plex_vault(changes: &ChangeSet) -> bool {
    changes.domain_rows.iter().any(|row| {
        let domain = match row {
            eve_db::sqlite_store::DomainRowChange::Upsert { domain, .. }
            | eve_db::sqlite_store::DomainRowChange::Delete { domain, .. } => *domain,
        };
        domain == eve_db::sqlite_store::DOMAIN_ACCOUNT_PLEX_VAULT
    })
}

fn skill_rank_from_sde(sde: &SdeRegistry, type_id: TypeId) -> f64 {
    sde.types
        .get(&type_id)
        .and_then(|t| {
            t.default_attributes
                .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                .copied()
        })
        .unwrap_or(1.0)
}

fn build_skill_info_keyval(sde: &SdeRegistry, type_id: TypeId, level: u8, sp: f64) -> PyRep {
    let lvl = i64::from(level);
    let rank = skill_rank_from_sde(sde, type_id);
    PyRep::keyval([
        ("typeID", PyRep::Integer(i64::from(type_id))),
        ("skillLevel", PyRep::Integer(lvl)),
        ("trainedSkillLevel", PyRep::Integer(lvl)),
        ("effectiveSkillLevel", PyRep::Integer(lvl)),
        ("activeSkillLevel", PyRep::Integer(lvl)),
        ("virtualSkillLevel", PyRep::None),
        ("skillPoints", PyRep::Real(sp)),
        ("trainedSkillPoints", PyRep::Real(sp)),
        ("skillRank", PyRep::Real(rank)),
    ])
}

fn build_skill_queue_list(queue: &[SkillQueueEntry], paused: bool, now_unix: u64) -> PyRep {
    let active_base_unix = if paused {
        None
    } else {
        queue
            .first()
            .and_then(|head| head.started_at_unix.or(Some(now_unix)))
    };
    let mut cur_ft = active_base_unix.map(unix_to_filetime);
    let entries = queue
        .iter()
        .enumerate()
        .map(|(idx, e)| {
            let (start_rep, end_rep) = match cur_ft {
                Some(start_ft) => {
                    let dur_ft = (e.duration_seconds.max(1.0) * 10_000_000.0) as i64;
                    let end_ft = start_ft + dur_ft;
                    cur_ft = Some(end_ft);
                    (PyRep::Integer(start_ft), PyRep::Integer(end_ft))
                }
                None => (PyRep::None, PyRep::None),
            };
            PyRep::keyval([
                ("trainingTypeID", PyRep::Integer(i64::from(e.type_id))),
                ("trainingToLevel", PyRep::Integer(i64::from(e.target_level))),
                ("trainingStartSP", PyRep::Real(e.start_sp)),
                ("trainingDestinationSP", PyRep::Real(e.end_sp)),
                ("durationSeconds", PyRep::Real(e.duration_seconds)),
                ("queuePosition", PyRep::Integer(idx as i64)),
                ("trainingStartTime", start_rep),
                ("trainingEndTime", end_rep),
            ])
        })
        .collect();
    PyRep::List(entries)
}

fn project_skill_changes(input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
    let now_ft = input.now_filetime();

    // 1. Trained skills -> `OnServerSkillsChanged` & `OnServerSkillsRemoved`.
    for change in &input.changes.skills {
        let char_id = change.key;
        let before_map_opt: Option<&HashMap<TypeId, TrainedSkillRecord>> = change.before.as_deref();
        let after_map_opt: Option<&HashMap<TypeId, TrainedSkillRecord>> = change.after.as_deref();
        if before_map_opt.is_none() && after_map_opt.is_none() {
            continue;
        }

        let mut changed_entries: Vec<(PyRep, PyRep)> = Vec::new();
        let mut removed_entries: Vec<(PyRep, PyRep)> = Vec::new();

        if let Some(after_map) = after_map_opt {
            let mut tids: Vec<TypeId> = after_map.keys().copied().collect();
            tids.sort_unstable();
            for tid in tids {
                let after_rec = &after_map[&tid];
                let changed = before_map_opt
                    .and_then(|b| b.get(&tid))
                    .is_none_or(|b| b != after_rec);
                if changed {
                    changed_entries.push((
                        PyRep::Integer(i64::from(tid)),
                        build_skill_info_keyval(
                            input.sde,
                            tid,
                            after_rec.level,
                            after_rec.skill_points,
                        ),
                    ));
                }
            }
        }

        if let Some(before_map) = before_map_opt {
            let mut removed_tids: Vec<TypeId> = before_map
                .keys()
                .filter(|tid| after_map_opt.is_none_or(|a| !a.contains_key(tid)))
                .copied()
                .collect();
            removed_tids.sort_unstable();
            for tid in removed_tids {
                let old_rec = &before_map[&tid];
                removed_entries.push((
                    PyRep::Integer(i64::from(tid)),
                    build_skill_info_keyval(input.sde, tid, old_rec.level, old_rec.skill_points),
                ));
            }
        }

        if !changed_entries.is_empty() {
            sink.push(ProjectedNotification::from_typed(
                ProjectedTarget::Character(char_id),
                OnServerSkillsChanged {
                    skill_infos: PyRep::Dict(changed_entries),
                    event: PyRep::None,
                    timestamp: now_ft,
                },
            ));
        }
        if !removed_entries.is_empty() {
            sink.push(ProjectedNotification::from_typed(
                ProjectedTarget::Character(char_id),
                OnServerSkillsRemoved {
                    skill_infos: PyRep::Dict(removed_entries),
                    timestamp: now_ft,
                },
            ));
        }
    }

    // 2. Free skill points -> `OnFreeSkillPointsChanged`.
    for change in &input.changes.characters {
        if let (Some(before), Some(after)) = (change.before.as_deref(), change.after.as_deref()) {
            let char_id = change.key;
            if (before.free_skill_points - after.free_skill_points).abs() > 1e-6 {
                sink.push(ProjectedNotification::from_typed(
                    ProjectedTarget::Character(char_id),
                    OnFreeSkillPointsChanged {
                        new_free_skill_points: after.free_skill_points,
                    },
                ));
            }
        }
    }

    // 3. Skill queue updates -> `OnNewSkillQueueSaved`.
    for change in &input.changes.skill_queues {
        if change.before.is_none() && change.after.is_none() {
            continue;
        }
        let char_id = change.key;
        let queue_slice: &[SkillQueueEntry] =
            change.after.as_deref().map(Vec::as_slice).unwrap_or(&[]);
        let paused = input.after.paused_skill_queues.contains(&char_id);
        let queue_rep = build_skill_queue_list(queue_slice, paused, input.now_unix);
        sink.push(ProjectedNotification::from_typed(
            ProjectedTarget::Character(char_id),
            OnNewSkillQueueSaved {
                new_queue: queue_rep,
            },
        ));
    }

    // 4. Newly paused queues (when `before` snapshot is supplied).
    if input.changes.cold_domains.d6_batch4 {
        if let Some(before) = input.before {
            let mut newly_paused: Vec<i64> = input
                .after
                .paused_skill_queues
                .iter()
                .filter(|cid| !before.paused_skill_queues.contains(cid))
                .copied()
                .collect();
            newly_paused.sort_unstable();
            for char_id in newly_paused {
                sink.push(ProjectedNotification::from_typed(
                    ProjectedTarget::Character(char_id),
                    OnSkillQueuePausedServer { paused: None },
                ));
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct StoredOrderMetaJson {
    range: i64,
    duration: i64,
    issue_unix: u64,
    order_state: Option<u8>,
    is_corp: Option<bool>,
}

struct ResolvedOrderMeta {
    range: i64,
    duration: i64,
    issue_unix: u64,
    order_state: Option<i64>,
    is_corp: Option<bool>,
}

fn load_projected_order_meta(
    snap: &WorldStateSnapshot,
    order: &MarketOrderRecord,
    default_unix: u64,
) -> ResolvedOrderMeta {
    let nid = -(4_000_000_000_000_i64.saturating_add(order.order_id.abs()));
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(parsed) = serde_json::from_str::<StoredOrderMetaJson>(&note.note) {
            return ResolvedOrderMeta {
                range: parsed.range,
                duration: if parsed.duration > 0 {
                    parsed.duration
                } else {
                    90
                },
                issue_unix: if parsed.issue_unix > 0 {
                    parsed.issue_unix
                } else {
                    default_unix
                },
                order_state: parsed.order_state.map(i64::from),
                is_corp: parsed.is_corp,
            };
        }
    }
    if let Some(m) = snap.market_order_meta.get(&order.order_id) {
        let order_state = match m.state.as_deref() {
            Some("expired") => Some(4),
            Some("cancelled") => Some(3),
            Some("closed") => Some(2),
            _ => None,
        };
        let range = match m.range.as_str() {
            "station" => -1,
            "solarsystem" => 0,
            "region" => 32767,
            s => s.parse().unwrap_or(if order.is_bid { -1 } else { 32767 }),
        };
        return ResolvedOrderMeta {
            range,
            duration: m.duration_days,
            issue_unix: m.issued_unix,
            order_state,
            is_corp: Some(m.is_corporation),
        };
    }
    ResolvedOrderMeta {
        range: if order.is_npc || !order.is_bid {
            32767
        } else {
            -1
        },
        duration: if order.is_npc { 365 } else { 90 },
        issue_unix: default_unix,
        order_state: None,
        is_corp: None,
    }
}

fn resolve_station_solar_and_constellation(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    station_id: i64,
) -> (u32, u32) {
    let sys_id = snap
        .structures
        .get(&station_id)
        .map(|st| st.solar_system_id)
        .or_else(|| sde.stations.get(&station_id).map(|st| st.solar_system_id))
        .or_else(|| {
            sde.solar_systems.values().find_map(|sys| {
                sys.stations
                    .iter()
                    .any(|s| s.station_id == station_id)
                    .then_some(sys.solar_system_id)
            })
        })
        .unwrap_or(0);
    let const_id = sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.constellation_id)
        .unwrap_or(0);
    (sys_id, const_id)
}

fn build_own_order_keyval(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    order: &MarketOrderRecord,
    meta: &ResolvedOrderMeta,
    is_corp: bool,
    order_state: i64,
) -> PyRep {
    let (sys_id, const_id) = resolve_station_solar_and_constellation(snap, sde, order.station_id);
    PyRep::keyval([
        ("orderID", PyRep::Integer(order.order_id)),
        ("typeID", PyRep::Integer(i64::from(order.type_id))),
        ("bid", PyRep::Bool(order.is_bid)),
        ("duration", PyRep::Integer(meta.duration)),
        (
            "volRemaining",
            PyRep::Integer(i64::from(order.vol_remaining)),
        ),
        ("volEntered", PyRep::Integer(i64::from(order.vol_entered))),
        ("minVolume", PyRep::Integer(i64::from(order.min_volume))),
        ("price", PyRep::Real(order.price.to_wire_f64())),
        ("stationID", PyRep::Integer(order.station_id)),
        ("regionID", PyRep::Integer(i64::from(order.region_id))),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("constellationID", PyRep::Integer(i64::from(const_id))),
        ("range", PyRep::Integer(meta.range)),
        (
            "issueDate",
            PyRep::Integer(unix_to_filetime(meta.issue_unix)),
        ),
        ("isCorp", PyRep::Bool(is_corp)),
        ("orderState", PyRep::Integer(order_state)),
    ])
}

fn is_corp_market_order(
    order: &MarketOrderRecord,
    meta: &ResolvedOrderMeta,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
) -> bool {
    if let Some(flag) = meta.is_corp {
        return flag;
    }
    if let Some(m) = after
        .market_order_meta
        .get(&order.order_id)
        .or_else(|| before.and_then(|b| b.market_order_meta.get(&order.order_id)))
    {
        return m.is_corporation;
    }
    is_known_corporation(order.owner_id, after, before)
        && !is_known_character(order.owner_id, after, before)
}

fn project_market_order_changes(input: &DeltaProjectionInput<'_>, sink: &mut dyn DeltaSink) {
    if input.changes.market_orders.is_empty() {
        return;
    }

    let mut grouped: BTreeMap<(ProjectedTarget, &'static str, bool), Vec<PyRep>> = BTreeMap::new();

    for change in &input.changes.market_orders {
        match (change.before.as_deref(), change.after.as_deref()) {
            (None, Some(order)) => {
                if order.is_npc || order.owner_id <= 0 {
                    continue;
                }
                let meta = load_projected_order_meta(input.after, order, input.now_unix);
                let is_corp = is_corp_market_order(order, &meta, input.after, input.before);
                let target = if is_corp {
                    ProjectedTarget::Corporation(order.owner_id)
                } else {
                    ProjectedTarget::Character(order.owner_id)
                };
                let state = meta
                    .order_state
                    .unwrap_or(if order.vol_remaining > 0 { 1 } else { 2 });
                let kv =
                    build_own_order_keyval(input.after, input.sde, order, &meta, is_corp, state);
                grouped
                    .entry((target, "Add", is_corp))
                    .or_default()
                    .push(kv);
            }
            (Some(old), Some(new)) => {
                if new.is_npc || new.owner_id <= 0 || old == new {
                    continue;
                }
                let meta = load_projected_order_meta(input.after, new, input.now_unix);
                let is_corp = is_corp_market_order(new, &meta, input.after, input.before);
                let target = if is_corp {
                    ProjectedTarget::Corporation(new.owner_id)
                } else {
                    ProjectedTarget::Character(new.owner_id)
                };
                let state = meta
                    .order_state
                    .unwrap_or(if new.vol_remaining > 0 { 1 } else { 2 });
                let reason = if state == 4 { "Expiry" } else { "Modify" };
                let kv = build_own_order_keyval(input.after, input.sde, new, &meta, is_corp, state);
                grouped
                    .entry((target, reason, is_corp))
                    .or_default()
                    .push(kv);
            }
            (Some(old), None) => {
                if old.is_npc || old.owner_id <= 0 {
                    continue;
                }
                let meta = load_projected_order_meta(input.after, old, input.now_unix);
                let is_corp = is_corp_market_order(old, &meta, input.after, input.before);
                let target = if is_corp {
                    ProjectedTarget::Corporation(old.owner_id)
                } else {
                    ProjectedTarget::Character(old.owner_id)
                };
                let mut closed = old.clone();
                closed.vol_remaining = 0;
                let state = meta.order_state.unwrap_or(2);
                let reason = if state == 4 { "Expiry" } else { "Modify" };
                let kv =
                    build_own_order_keyval(input.after, input.sde, &closed, &meta, is_corp, state);
                grouped
                    .entry((target, reason, is_corp))
                    .or_default()
                    .push(kv);
            }
            (None, None) => {}
        }
    }

    for ((target, reason, is_corp), orders) in grouped {
        sink.push(ProjectedNotification::from_typed_with_id_type(
            target,
            target.default_id_type(),
            OnOwnOrdersChanged {
                orders: PyRep::List(orders),
                reason: PyRep::String(reason.into()),
                is_corp: PyRep::Bool(is_corp),
            },
        ));
    }
}

/// Projects attribute differences between two [`FitProjectionSnapshot`]s into `OnModuleAttributeChanges`.
pub fn project_fit_attribute_diff(
    char_id: i64,
    ship_id: i64,
    before: &FitProjectionSnapshot,
    after: &FitProjectionSnapshot,
    now_filetime: i64,
) -> Option<ProjectedNotification> {
    let mut entries = Vec::new();
    append_attr_map_diff(
        &mut entries,
        char_id,
        ship_id,
        &before.ship_attributes,
        &after.ship_attributes,
        now_filetime,
    );

    let mut module_ids: BTreeSet<i64> = before.module_attributes.keys().copied().collect();
    module_ids.extend(after.module_attributes.keys().copied());
    for module_id in module_ids {
        let old_map = before.module_attributes.get(&module_id);
        let new_map = after.module_attributes.get(&module_id);
        if let (Some(old_attrs), Some(new_attrs)) = (old_map, new_map) {
            append_attr_map_diff(
                &mut entries,
                char_id,
                module_id,
                old_attrs,
                new_attrs,
                now_filetime,
            );
        }
    }

    if entries.is_empty() {
        return None;
    }

    Some(ProjectedNotification::from_typed(
        ProjectedTarget::Character(char_id),
        OnModuleAttributeChanges {
            changes: PyRep::List(entries),
        },
    ))
}

fn append_attr_map_diff(
    out: &mut Vec<PyRep>,
    char_id: i64,
    entity_id: i64,
    before: &BTreeMap<u16, f64>,
    after: &BTreeMap<u16, f64>,
    now_filetime: i64,
) {
    let mut attr_ids: BTreeSet<u16> = before.keys().copied().collect();
    attr_ids.extend(after.keys().copied());
    for attr_id in attr_ids {
        let old_val = before.get(&attr_id).copied().unwrap_or(0.0);
        let new_val = after.get(&attr_id).copied().unwrap_or(0.0);
        if new_val.is_finite() && old_val.is_finite() && (new_val - old_val).abs() > 1e-9 {
            out.push(PyRep::Tuple(vec![
                PyRep::String("OnModuleAttributeChange".into()),
                PyRep::Integer(char_id),
                PyRep::Integer(entity_id),
                PyRep::Integer(i64::from(attr_id)),
                PyRep::Integer(now_filetime),
                PyRep::Real(new_val),
                PyRep::Real(old_val),
                PyRep::None,
            ]));
        }
    }
}

/// Resolves all online `(char_id, user_id)` pairs for a [`ProjectedTarget`] from snapshots + online lookup.
pub fn resolve_projected_recipients(
    target: ProjectedTarget,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
    online: &impl OnlineCharacterLookup,
) -> Vec<(i64, Option<u64>)> {
    let mut candidates: Vec<(i64, u64)> = Vec::new();
    match target {
        ProjectedTarget::Character(char_id) => {
            if let Some(ch) = after
                .characters
                .get(&char_id)
                .or_else(|| before.and_then(|b| b.characters.get(&char_id)))
            {
                candidates.push((char_id, ch.user_id));
            }
        }
        ProjectedTarget::Corporation(corp_id) => {
            for ch in after.characters.values() {
                if ch.corporation_id == corp_id {
                    candidates.push((ch.character_id, ch.user_id));
                }
            }
            if let Some(corp) = after.corporations.get(&corp_id) {
                for &member_id in &corp.members {
                    if let Some(ch) = after
                        .characters
                        .get(&member_id)
                        .or_else(|| before.and_then(|b| b.characters.get(&member_id)))
                    {
                        candidates.push((member_id, ch.user_id));
                    }
                }
            }
        }
    }

    let mut out: Vec<(i64, Option<u64>)> = candidates
        .into_iter()
        .filter(|&(cid, uid)| online.is_character_online(cid, uid))
        .map(|(cid, uid)| (cid, Some(uid)))
        .collect();
    out.sort_by_key(|(cid, _)| *cid);
    out.dedup_by_key(|(cid, _)| *cid);
    out
}

/// Expands a list of [`ProjectedNotification`]s into per-online-character [`ResolvedNotification`]s.
pub fn resolve_all_notifications(
    notifications: Vec<ProjectedNotification>,
    after: &WorldStateSnapshot,
    before: Option<&WorldStateSnapshot>,
    online: &impl OnlineCharacterLookup,
) -> Vec<ResolvedNotification> {
    let mut resolved = Vec::new();
    for notif in notifications {
        let recipients = resolve_projected_recipients(notif.target, after, before, online);
        for (char_id, user_id) in recipients {
            resolved.push(ResolvedNotification {
                recipient_char_id: char_id,
                recipient_user_id: user_id,
                event: notif.event,
                id_type: notif.id_type,
                args: notif.args.clone(),
            });
        }
    }
    resolved
}

/// Encodes [`ResolvedNotification`]s into wire-ready [`PendingMachoNotification`] packets
/// through `router::notify_registry`.
pub fn materialize_pending_notifications(
    resolved: Vec<ResolvedNotification>,
    now_filetime: i64,
) -> Vec<PendingMachoNotification> {
    resolved
        .into_iter()
        .filter_map(|notif| {
            build_pending_registered_notification(
                notif.recipient_char_id,
                notif.recipient_user_id,
                notif.event,
                notif.id_type,
                notif.args,
                now_filetime,
            )
        })
        .collect()
}
