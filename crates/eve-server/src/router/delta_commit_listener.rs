//! Commit-time wallet/PLEX delivery & explicit in-transaction effect bridge (`#931` Tracks 1–2).
//!
//! Wires [`DeltaStateCache`] + [`DefaultDeltaProjection::wallets_only`] into
//! `WorldRepository::register_post_commit_listener` so committed `ChangeSet` deltas on
//! personal ISK (`"cash"`), corp divisions `1..=7` (`"cash"` / `"cash2".."cash7"`), and PLEX
//! (`"plex"`) materialize `OnAccountChange` notifications alongside switched item projection.

#![forbid(unsafe_code)]

use crate::router::cache_notifications::{
    assemble_pending_notification, current_dispatch_staging_scope_id,
    current_dispatch_staging_scope_liveness, enqueue_or_stage_pending_batch,
    find_staged_notification_by_seq, insert_before_staged_seq_or_stage, is_active_dispatch_char,
    next_notification_seq_id, pending_notifications_ptr, push_pending_notification_capped,
    try_stage_dispatch_notification, with_matching_dispatch_transitioned_chars, NotificationTarget,
    PendingMachoNotification, MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR,
    MAX_PENDING_NOTIFICATIONS_PER_CHAR,
};
use crate::router::context::ServerContext;
use crate::router::delta_projection::{
    materialize_pending_notifications, resolve_all_notifications, DefaultDeltaProjection,
    DeltaProjectionHook, DeltaSink, DeltaStateCache, OnlineCharacterLookup, PlexDelta,
    ProjectedNotification, ProjectedTarget, ResolvedNotification,
};
use crate::router::notify_registry::OnAccountChange;
use eve_db::{
    ChangeSet, Effects, TxNotificationEffect, TxNotificationTarget, WorldRepository,
    WorldStateSnapshot,
};
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, Weak};

const MAX_RECENT_COMMIT_EPOCH_LAG: u64 = 32;
const MAX_RECENT_PROJECTED_RECORDS: usize = 4096;
const MAX_OUTSIDE_COVERED_MONITORS: usize = 512;
const MAX_DISPATCH_COVERED_MONITOR_SCOPES: usize = 512;

type CoveredMonitorMap = HashMap<i64, (i64, i64, HashSet<u32>)>;

#[derive(Debug, Clone)]
struct ProjectedAccountRecord {
    char_id: i64,
    account_key: String,
    owner_id: i64,
    balance: f64,
    seq_id: u64,
    commit_epoch: u64,
    staging_scope_id: Option<u64>,
}

#[derive(Debug, Default)]
struct EpochLedger {
    records: VecDeque<ProjectedAccountRecord>,
    /// Monotonic commit counter per `(owner_id, account_key)` so unrelated DB
    /// commits (career goals, telemetry, other wallets) never evict recent records.
    wallet_epochs: HashMap<i64, HashMap<String, u64>>,
    dispatch_covered_monitors: HashMap<u64, (Weak<()>, CoveredMonitorMap)>,
    dispatch_monitor_scope_order: VecDeque<u64>,
    outside_covered_monitors: CoveredMonitorMap,
    outside_covered_monitor_order: VecDeque<i64>,
}

impl EpochLedger {
    /// Record a batch of committed wallet account notifications under per-wallet
    /// epochs and prune records older than `MAX_RECENT_COMMIT_EPOCH_LAG` for those keys.
    fn record_batch(&mut self, mut committed_records: Vec<ProjectedAccountRecord>) {
        if committed_records.is_empty() {
            return;
        }
        let mut batch_epochs: HashMap<i64, HashMap<String, u64>> = HashMap::new();
        for rec in &mut committed_records {
            let next_epoch = if let Some(&epoch) = batch_epochs
                .get(&rec.owner_id)
                .and_then(|m| m.get(&rec.account_key))
            {
                epoch
            } else {
                let by_key = self.wallet_epochs.entry(rec.owner_id).or_default();
                let next = if let Some(entry) = by_key.get_mut(&rec.account_key) {
                    *entry = entry.saturating_add(1);
                    *entry
                } else {
                    by_key.insert(rec.account_key.clone(), 1);
                    1
                };
                batch_epochs
                    .entry(rec.owner_id)
                    .or_default()
                    .insert(rec.account_key.clone(), next);
                next
            };
            rec.commit_epoch = next_epoch;
        }
        self.records.retain(|r| {
            let Some(&latest) = batch_epochs
                .get(&r.owner_id)
                .and_then(|m| m.get(&r.account_key))
            else {
                return true;
            };
            latest.saturating_sub(r.commit_epoch) <= MAX_RECENT_COMMIT_EPOCH_LAG
        });
        for rec in committed_records {
            if self.records.len() >= MAX_RECENT_PROJECTED_RECORDS {
                self.records.pop_front();
            }
            self.records.push_back(rec);
        }
    }

    fn wallet_epoch(&self, owner_id: i64, account_key: &str) -> u64 {
        self.wallet_epochs
            .get(&owner_id)
            .and_then(|m| m.get(account_key))
            .copied()
            .unwrap_or(0)
    }

    fn prune_dead_dispatch_scopes(&mut self) {
        self.dispatch_covered_monitors
            .retain(|_, (w, monitors)| w.strong_count() > 0 && !monitors.is_empty());
        self.dispatch_monitor_scope_order
            .retain(|id| self.dispatch_covered_monitors.contains_key(id));
    }

    fn invalidate_all_covered_monitors(
        &mut self,
        touched_types: &HashMap<i64, HashSet<u32>>,
        excluded_chars: &[i64],
    ) {
        self.prune_dead_dispatch_scopes();
        for (_, monitors) in self.dispatch_covered_monitors.values_mut() {
            invalidate_covered_monitors_for_types(monitors, None, touched_types);
            retain_monitors_excluding_chars(monitors, None, excluded_chars);
        }
        self.prune_dead_dispatch_scopes();
        invalidate_covered_monitors_for_types(
            &mut self.outside_covered_monitors,
            Some(&mut self.outside_covered_monitor_order),
            touched_types,
        );
        retain_monitors_excluding_chars(
            &mut self.outside_covered_monitors,
            Some(&mut self.outside_covered_monitor_order),
            excluded_chars,
        );
    }

    fn record_dispatch_covered_monitors(
        &mut self,
        scope_id: u64,
        liveness: Weak<()>,
        covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
        excluded_chars: &[i64],
    ) {
        if covered.is_empty() || liveness.strong_count() == 0 {
            return;
        }
        self.prune_dead_dispatch_scopes();
        let had_scope = self.dispatch_covered_monitors.contains_key(&scope_id);
        let entry = self
            .dispatch_covered_monitors
            .entry(scope_id)
            .or_insert_with(|| (liveness, HashMap::new()));
        merge_covered_industry_monitors(&mut entry.1, None, None, covered, excluded_chars);
        if entry.1.is_empty() {
            self.dispatch_covered_monitors.remove(&scope_id);
            self.dispatch_monitor_scope_order
                .retain(|&id| id != scope_id);
            return;
        }
        if had_scope {
            self.dispatch_monitor_scope_order
                .retain(|&id| id != scope_id);
        }
        self.dispatch_monitor_scope_order.push_back(scope_id);
        while self.dispatch_covered_monitors.len() > MAX_DISPATCH_COVERED_MONITOR_SCOPES {
            let Some(oldest_scope) = self.dispatch_monitor_scope_order.pop_front() else {
                break;
            };
            self.dispatch_covered_monitors.remove(&oldest_scope);
        }
    }

    fn record_outside_covered_monitors(
        &mut self,
        covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
        excluded_chars: &[i64],
    ) {
        if covered.is_empty() {
            return;
        }
        merge_covered_industry_monitors(
            &mut self.outside_covered_monitors,
            Some(&mut self.outside_covered_monitor_order),
            Some(MAX_OUTSIDE_COVERED_MONITORS),
            covered,
            excluded_chars,
        );
    }

    fn clear_monitors_for_chars(&mut self, chars: &[i64]) {
        if chars.is_empty() {
            return;
        }
        self.prune_dead_dispatch_scopes();
        for (_, monitors) in self.dispatch_covered_monitors.values_mut() {
            retain_monitors_excluding_chars(monitors, None, chars);
        }
        self.prune_dead_dispatch_scopes();
        retain_monitors_excluding_chars(
            &mut self.outside_covered_monitors,
            Some(&mut self.outside_covered_monitor_order),
            chars,
        );
    }

    fn remove_monitor_by_id(&mut self, monitor_id: i64) {
        self.prune_dead_dispatch_scopes();
        for (_, monitors) in self.dispatch_covered_monitors.values_mut() {
            monitors.remove(&monitor_id);
        }
        self.prune_dead_dispatch_scopes();
        if self.outside_covered_monitors.remove(&monitor_id).is_some() {
            self.outside_covered_monitor_order
                .retain(|&id| id != monitor_id);
        }
    }

    fn is_monitor_covered(
        &mut self,
        scope_id: Option<u64>,
        monitor_id: i64,
        tids: &[u32],
    ) -> bool {
        if tids.is_empty() {
            return false;
        }
        self.prune_dead_dispatch_scopes();
        let monitors = match scope_id {
            Some(sid) => match self.dispatch_covered_monitors.get(&sid) {
                Some((_, map)) => map,
                None => return false,
            },
            None => &self.outside_covered_monitors,
        };
        monitors
            .get(&monitor_id)
            .is_some_and(|(_, _, types)| tids.iter().all(|tid| types.contains(tid)))
    }
}

#[derive(Debug, Default)]
pub(crate) struct TxItemEffectKeys {
    item_keys: HashSet<(i64, i64)>,
    blueprint_keys: HashSet<(i64, i64)>,
}

impl TxItemEffectKeys {
    fn observe_item_rows(&mut self, char_id: i64, rows: &[PyRep]) {
        for entry in rows {
            let row = match entry {
                PyRep::Tuple(pair) | PyRep::List(pair) => pair.first(),
                other => Some(other),
            };
            if let Some(PyRep::Integer(item_id)) = row.and_then(|r| r.dbrow_get("itemID")) {
                self.item_keys.insert((char_id, *item_id));
            }
        }
    }

    fn observe(&mut self, char_id: i64, event: &str, payload: &[PyRep]) {
        match event {
            "OnItemChange" => {
                if let Some(PyRep::Integer(item_id)) =
                    payload.first().and_then(|r| r.dbrow_get("itemID"))
                {
                    self.item_keys.insert((char_id, *item_id));
                }
            }
            "OnMultipleItemChange" | "OnItemsChanged" => {
                if let Some(PyRep::List(rows) | PyRep::Tuple(rows)) = payload.first() {
                    self.observe_item_rows(char_id, rows);
                }
            }
            "OnBlueprintsUpdated" => {
                if let [PyRep::Integer(owner_id), ..] = payload {
                    self.blueprint_keys.insert((char_id, *owner_id));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn contains_item(&self, char_id: i64, item_id: i64) -> bool {
        self.item_keys.contains(&(char_id, item_id))
    }

    pub(crate) fn contains_blueprint(&self, char_id: i64, owner_id: i64) -> bool {
        self.blueprint_keys.contains(&(char_id, owner_id))
    }
}

const COMMIT_BRIDGES_SLOT_KEY: &str = "router_delta_commit_bridges";

struct DeltaCommitBridgeState {
    recent_epoch: Mutex<EpochLedger>,
}

struct CommitBridgesRepoSlot {
    bridges: Mutex<HashMap<usize, Weak<DeltaCommitBridgeState>>>,
}

impl CommitBridgesRepoSlot {
    fn new() -> Self {
        Self {
            bridges: Mutex::new(HashMap::new()),
        }
    }
}

fn commit_bridges_slot(db: &eve_db::WorldRepository) -> Arc<CommitBridgesRepoSlot> {
    db.get_or_init_transient_runtime_slot(COMMIT_BRIDGES_SLOT_KEY, CommitBridgesRepoSlot::new)
}

struct PlexAccountChangeHook {
    resolve_user_char: Box<dyn Fn(u64) -> Option<i64> + Send + Sync>,
}

impl DeltaProjectionHook for PlexAccountChangeHook {
    fn on_plex_delta(&self, delta: PlexDelta, sink: &mut dyn DeltaSink) {
        let Some(cid) = (self.resolve_user_char)(delta.user_id).or(delta.char_id) else {
            return;
        };
        sink.push(ProjectedNotification::from_typed(
            ProjectedTarget::Character(cid),
            OnAccountChange {
                account_key: "plex".into(),
                owner_id: cid,
                balance: delta.new_plex as f64,
            },
        ));
    }
}

struct AllCharactersOnline;

impl OnlineCharacterLookup for AllCharactersOnline {
    fn is_character_online(&self, _char_id: i64, _user_id: u64) -> bool {
        true
    }
}

fn extract_account_change_fields(payload_args: &[PyRep]) -> Option<(String, i64, f64)> {
    match payload_args {
        [PyRep::String(key) | PyRep::Token(key), PyRep::Integer(owner_id), bal_rep, ..] => {
            let bal = match bal_rep {
                PyRep::Real(v) => *v,
                PyRep::Integer(v) => *v as f64,
                _ => return None,
            };
            Some((key.clone(), *owner_id, bal))
        }
        _ => None,
    }
}

#[derive(Debug, Default)]
struct EmittedEffectKeys {
    account_keys: HashSet<(i64, Option<(String, i64)>)>,
    item_effects: TxItemEffectKeys,
}

fn resolve_tx_effect_recipients(
    db: &WorldRepository,
    snap: &WorldStateSnapshot,
    effect: &TxNotificationEffect,
) -> Vec<(i64, Option<u64>)> {
    let mut candidates: Vec<(i64, u64)> = Vec::new();
    match effect.target {
        TxNotificationTarget::Character(cid) => {
            if let Some(c) = snap.characters.get(&cid) {
                candidates.push((cid, c.user_id));
            }
        }
        TxNotificationTarget::Corporation(corp_id) => {
            for c in snap.characters.values() {
                if c.corporation_id == corp_id {
                    candidates.push((c.character_id, c.user_id));
                }
            }
        }
        TxNotificationTarget::Alliance(ally_id) => {
            for c in snap.characters.values() {
                if c.alliance_id == Some(ally_id) {
                    candidates.push((c.character_id, c.user_id));
                }
            }
        }
        TxNotificationTarget::SolarSystem(sys_id) => {
            for c in snap.characters.values() {
                if c.solar_system_id == sys_id {
                    candidates.push((c.character_id, c.user_id));
                }
            }
        }
    }
    let mut recipients: Vec<(i64, Option<u64>)> = candidates
        .into_iter()
        .filter(|&(cid, uid)| {
            db.get_online_session(uid)
                .is_some_and(|os| os.character_id == Some(cid))
        })
        .map(|(cid, uid)| (cid, Some(uid)))
        .collect();
    recipients.sort_by_key(|(cid, _)| *cid);
    recipients.dedup_by_key(|(cid, _)| *cid);
    recipients
}

fn dispatch_tx_effects(
    db: &WorldRepository,
    pend: &dyn eve_db::lock_rank::AnyRwLock<HashMap<i64, Vec<PendingMachoNotification>>>,
    notify_tx: &tokio::sync::broadcast::Sender<(i64, PendingMachoNotification)>,
    snap: &WorldStateSnapshot,
    effects: &Effects,
    now_ft: i64,
    commit_epoch: u64,
    staging_scope_id: Option<u64>,
    active_fuel_reserves: &crate::services::tactical_modules::ActiveFuelReserveTracker,
    db_key: usize,
) -> (EmittedEffectKeys, Vec<ProjectedAccountRecord>) {
    let mut emitted_effects = EmittedEffectKeys::default();
    let mut records = Vec::new();
    let mut built = Vec::new();

    for effect in &effects.notifications {
        let recipients = resolve_tx_effect_recipients(db, snap, effect);
        if recipients.is_empty() {
            continue;
        }
        active_fuel_reserves.observe_pre_emit_event(db_key, &effect.event, &effect.args);
        let is_account_change = effect.event == "OnAccountChange";
        let fields_opt = if is_account_change {
            extract_account_change_fields(&effect.args)
        } else {
            None
        };
        let suppression_key = fields_opt
            .as_ref()
            .map(|(k, owner_id, _)| (k.clone(), *owner_id));
        if !recipients.is_empty() {
            #[cfg(any(test, feature = "testkit"))]
            crate::router::notify_registry::assert_registered_event_shape_if_registered(
                &effect.event,
                &effect.args,
            );
        }
        for (cid, uid) in recipients {
            let notif = assemble_pending_notification(
                cid,
                uid,
                &effect.event,
                &effect.id_type,
                effect.args.clone(),
                now_ft,
            );
            if is_account_change {
                emitted_effects
                    .account_keys
                    .insert((cid, suppression_key.clone()));
            } else {
                emitted_effects
                    .item_effects
                    .observe(cid, &effect.event, &effect.args);
            }
            if let Some((ref account_key, owner_id, balance)) = fields_opt {
                records.push(ProjectedAccountRecord {
                    char_id: cid,
                    account_key: account_key.clone(),
                    owner_id,
                    balance,
                    seq_id: notif.seq_id,
                    commit_epoch,
                    staging_scope_id,
                });
            }
            built.push(notif);
        }
    }

    if !built.is_empty() {
        enqueue_or_stage_pending_batch(pend, notify_tx, &built);
    }
    (emitted_effects, records)
}

fn dedup_resolved_account_changes(resolved: Vec<ResolvedNotification>) -> Vec<ResolvedNotification> {
    let mut out: Vec<Option<ResolvedNotification>> = Vec::with_capacity(resolved.len());
    let mut seen: HashMap<(i64, String, i64), usize> = HashMap::new();
    for notif in resolved {
        if let Some((account_key, owner_id, _)) = extract_account_change_fields(&notif.args) {
            let dedup_key = (notif.recipient_char_id, account_key, owner_id);
            if let Some(&prev_idx) = seen.get(&dedup_key) {
                out[prev_idx] = None;
            }
            seen.insert(dedup_key, out.len());
        }
        out.push(Some(notif));
    }
    out.into_iter().flatten().collect()
}

fn project_wallet_notifs_for_commit(
    cache: &DeltaStateCache,
    projector: &DefaultDeltaProjection,
    snap: &WorldStateSnapshot,
    changes: &ChangeSet,
    sde: &SdeRegistry,
    now_unix: u64,
    online: &impl OnlineCharacterLookup,
) -> Vec<ResolvedNotification> {
    let projected = cache.project_with_cache(projector, None, snap, changes, sde, now_unix, &[]);
    let resolved = resolve_all_notifications(projected, snap, None, online);
    dedup_resolved_account_changes(resolved)
}

/// Registers the authoritative DB post-commit listener on `ctx.db`, dispatching explicit
/// `TxNotificationEffect`s and projecting committed wallet/corp/PLEX (`OnAccountChange`) and
/// item/blueprint/`OnIndustryMaterials` deltas (`#931`).
pub(crate) fn install_tx_post_commit_listener(ctx: &ServerContext) {
    let pend_ptr = pending_notifications_ptr(&*ctx.pending_notifications);
    let slot = commit_bridges_slot(&ctx.db);
    let bridge = {
        let mut map = slot.bridges.lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, w| w.strong_count() > 0);
        if map.get(&pend_ptr).and_then(Weak::upgrade).is_some() {
            return;
        }
        let state = Arc::new(DeltaCommitBridgeState {
            recent_epoch: Mutex::new(EpochLedger::default()),
        });
        map.insert(pend_ptr, Arc::downgrade(&state));
        state
    };

    let cache = Arc::new(DeltaStateCache::new());
    ctx.db.with_snapshot(|snap| cache.seed_from_snapshot(snap));

    let db_hook_w = Arc::downgrade(&ctx.db);
    let mut projector = DefaultDeltaProjection::wallets_only();
    projector.add_hook(Box::new(PlexAccountChangeHook {
        resolve_user_char: Box::new(move |user_id| {
            db_hook_w
                .upgrade()
                .and_then(|db| db.get_online_session(user_id))
                .and_then(|os| os.character_id)
        }),
    }));

    let db_w = Arc::downgrade(&ctx.db);
    let pend_w = Arc::downgrade(&ctx.pending_notifications);
    let notify_tx = ctx.notify_tx.clone();
    let sde = Arc::clone(&ctx.sde);
    let offline_bridge = Arc::downgrade(&bridge);
    let active_fuel_reserves = Arc::clone(&ctx.active_fuel_reserves);
    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);

    ctx.db
        .register_session_offline_listener(Arc::new(move |offline_chars| {
            if let Some(bridge) = offline_bridge.upgrade() {
                let mut ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
                ledger.clear_monitors_for_chars(offline_chars);
            }
        }));

    ctx.db.register_post_commit_listener(Arc::new(
        move |snap, changes, effects| {
            let (Some(db), Some(pend)) = (db_w.upgrade(), pend_w.upgrade()) else {
                return;
            };
            let now_ft = db.clock().now_blueos_filetime_ticks();
            let now_unix = db.now_unix_secs();
            // Assigned per `(owner_id, account_key)` inside `EpochLedger::record_batch`.
            let commit_epoch = 0u64;
            let staging_scope_id =
                current_dispatch_staging_scope_id(Some(&*pend), Some(&notify_tx));

            let (emitted_effects, mut committed_records) = if effects.notifications.is_empty() {
                (EmittedEffectKeys::default(), Vec::new())
            } else {
                dispatch_tx_effects(
                    &db,
                    &*pend,
                    &notify_tx,
                    snap,
                    effects,
                    now_ft,
                    commit_epoch,
                    staging_scope_id,
                    &active_fuel_reserves,
                    db_key,
                )
            };

            if !changes.is_empty() {
                let resolved = project_wallet_notifs_for_commit(
                    &cache,
                    &projector,
                    snap,
                    changes,
                    &sde,
                    now_unix,
                    &*db,
                );
                let filtered: Vec<ResolvedNotification> = resolved
                    .into_iter()
                    .filter(|r| {
                        if emitted_effects
                            .account_keys
                            .contains(&(r.recipient_char_id, None))
                        {
                            return false;
                        }
                        if let Some((key, owner_id, _)) = extract_account_change_fields(&r.args) {
                            if emitted_effects
                                .account_keys
                                .contains(&(r.recipient_char_id, Some((key, owner_id))))
                            {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();
                let materialized = materialize_pending_notifications(filtered, now_ft);
                if !materialized.is_empty() {
                    enqueue_or_stage_pending_batch(&*pend, &notify_tx, &materialized);
                    for notif in materialized {
                        if let Some((account_key, owner_id, balance)) =
                            extract_account_change_fields(&notif.args)
                        {
                            committed_records.push(ProjectedAccountRecord {
                                char_id: notif.recipient_char_id,
                                account_key,
                                owner_id,
                                balance,
                                seq_id: notif.seq_id,
                                commit_epoch,
                                staging_scope_id,
                            });
                        }
                    }
                }
            }

            if !changes.items.is_empty() || !changes.characters.is_empty() {
                let dispatch_scope =
                    current_dispatch_staging_scope_liveness(Some(&*pend), Some(&notify_tx));
                let in_dispatch = dispatch_scope.is_some();
                let touched_types = collect_touched_monitor_types(changes);
                let newly_transitioned: Vec<i64> = if !changes.characters.is_empty() {
                    let changed_chars =
                        crate::router::delta_item_switch::index_changed_characters(changes);
                    changed_chars
                        .keys()
                        .copied()
                        .filter(|&char_id| {
                            crate::router::delta_item_switch::is_character_scene_or_ship_transition(
                                char_id,
                                &changed_chars,
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                {
                    let mut ledger =
                        bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
                    ledger.invalidate_all_covered_monitors(&touched_types, &newly_transitioned);
                }

                let prior_transitioned = if in_dispatch {
                    with_matching_dispatch_transitioned_chars(
                        Some(&*pend),
                        Some(&notify_tx),
                        |chars| chars.clone(),
                    )
                    .unwrap_or_default()
                } else {
                    HashSet::new()
                };
                let batch =
                    crate::router::delta_item_switch::collect_switched_char_same_scene_item_specs(
                        in_dispatch,
                        &prior_transitioned,
                        effects.acting_char_id,
                        &*db,
                        snap,
                        changes,
                        &sde,
                        &emitted_effects.item_effects,
                        &active_fuel_reserves,
                        db_key,
                    );
                if in_dispatch && !newly_transitioned.is_empty() {
                    with_matching_dispatch_transitioned_chars(
                        Some(&*pend),
                        Some(&notify_tx),
                        |chars| chars.extend(newly_transitioned.iter().copied()),
                    );
                }
                if !batch.item_specs.is_empty() || !batch.blueprint_specs.is_empty() {
                    let built =
                        crate::router::delta_item_corp::build_switched_item_commit_notifications(
                            snap,
                            &sde,
                            &*db,
                            batch,
                            now_unix,
                            now_ft,
                            &active_fuel_reserves,
                            db_key,
                        );
                    if !built.covered_industry_monitors.is_empty() {
                        let mut ledger =
                            bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some((scope_id, liveness)) = dispatch_scope {
                            ledger.record_dispatch_covered_monitors(
                                scope_id,
                                liveness,
                                &built.covered_industry_monitors,
                                &newly_transitioned,
                            );
                        } else {
                            ledger.record_outside_covered_monitors(
                                &built.covered_industry_monitors,
                                &newly_transitioned,
                            );
                        }
                    }
                    if !built.notifs.is_empty() {
                        enqueue_or_stage_pending_batch(&*pend, &notify_tx, &built.notifs);
                    }
                }
            }

            if !committed_records.is_empty() {
                bridge
                    .recent_epoch
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .record_batch(committed_records);
            }
        },
    ));
}

/// Registers a post-commit listener projecting committed wallet / corp-division / PLEX Vault
/// deltas into a `PublicGatewayRouter` Macho notify sink callback (used by router unit tests
/// that instantiate `WorldRepository` without a full `ServerContext`).
pub fn install_wallet_delta_sink_listener<F>(
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    online_only: bool,
    sink: F,
) -> u64
where
    F: Fn(NotificationTarget, &str, &str, Vec<PyRep>) + Send + Sync + 'static,
{
    let cache = Arc::new(DeltaStateCache::new());
    db.with_snapshot(|snap| cache.seed_from_snapshot(snap));

    let user_char_fallback: Arc<Mutex<BTreeMap<u64, i64>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let db_hook_w = Arc::downgrade(db);
    let fallback_map = Arc::clone(&user_char_fallback);
    let mut projector = DefaultDeltaProjection::wallets_only();
    projector.add_hook(Box::new(PlexAccountChangeHook {
        resolve_user_char: Box::new(move |user_id| {
            db_hook_w
                .upgrade()
                .and_then(|repo| repo.get_online_session(user_id))
                .and_then(|os| os.character_id)
                .or_else(|| {
                    (!online_only)
                        .then(|| {
                            fallback_map
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .get(&user_id)
                                .copied()
                        })
                        .flatten()
                })
        }),
    }));

    let db_w = Arc::downgrade(db);
    let sde = Arc::clone(sde);
    let fallback_writer = Arc::clone(&user_char_fallback);
    db.register_post_commit_listener(Arc::new(move |snap, changes, _effects| {
        if changes.is_empty() {
            return;
        }
        let Some(db) = db_w.upgrade() else {
            return;
        };
        if !online_only {
            let mut guard = fallback_writer.lock().unwrap_or_else(|e| e.into_inner());
            guard.clear();
            for ch in snap.characters.values() {
                guard
                    .entry(ch.user_id)
                    .and_modify(|cid| {
                        if ch.character_id < *cid {
                            *cid = ch.character_id;
                        }
                    })
                    .or_insert(ch.character_id);
            }
        }
        let now_unix = db.now_unix_secs();
        let resolved = if online_only {
            project_wallet_notifs_for_commit(&cache, &projector, snap, changes, &sde, now_unix, &*db)
        } else {
            project_wallet_notifs_for_commit(
                &cache,
                &projector,
                snap,
                changes,
                &sde,
                now_unix,
                &AllCharactersOnline,
            )
        };
        for notif in resolved {
            sink(
                NotificationTarget::Character(notif.recipient_char_id),
                notif.event,
                notif.id_type,
                notif.args,
            );
        }
    }))
}

fn insert_before_pos_with_cap(
    queue: &mut Vec<PendingMachoNotification>,
    mut pos: usize,
    notif: PendingMachoNotification,
) {
    let cap = if is_active_dispatch_char(notif.recipient_char_id) {
        MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR
    } else {
        MAX_PENDING_NOTIFICATIONS_PER_CHAR
    };
    while queue.len() >= cap && queue.len() > 1 {
        if pos > 0 {
            queue.remove(0);
            pos -= 1;
        } else {
            queue.remove(1);
        }
    }
    queue.insert(pos, notif);
}

fn lookup_commit_bridge(ctx: &ServerContext) -> Option<Arc<DeltaCommitBridgeState>> {
    let pend_ptr = pending_notifications_ptr(&*ctx.pending_notifications);
    let slot = ctx
        .db
        .get_transient_runtime_slot::<CommitBridgesRepoSlot>(COMMIT_BRIDGES_SLOT_KEY)
        .flatten()?;
    let guard = slot.bridges.lock().unwrap_or_else(|e| e.into_inner());
    guard.get(&pend_ptr).and_then(Weak::upgrade)
}

#[cfg(test)]
fn commit_bridge_state(ctx: &ServerContext) -> Option<Arc<DeltaCommitBridgeState>> {
    lookup_commit_bridge(ctx)
}

#[cfg(test)]
fn commit_bridges_for_db(db: &eve_db::WorldRepository) -> Vec<Arc<DeltaCommitBridgeState>> {
    let Some(slot) = db
        .get_transient_runtime_slot::<CommitBridgesRepoSlot>(COMMIT_BRIDGES_SLOT_KEY)
        .flatten()
    else {
        return Vec::new();
    };
    let guard = slot.bridges.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .values()
        .filter_map(|w| w.upgrade())
        .collect()
}

/// Coordinates remaining unlocked manual `OnAccountChange` calls with same-epoch commit-projected
/// `OnAccountChange` notifications: deduplicates identical committed balances and preserves
/// pre-settlement intermediate balances ahead of the committed final balance.
#[track_caller]
pub(crate) fn try_handle_legacy_account_change_emit(
    ctx: &ServerContext,
    recipients: &[(i64, Option<u64>)],
    id_type: &str,
    payload_args: &[PyRep],
    now_ft: i64,
) -> Option<Vec<PendingMachoNotification>> {
    if recipients.is_empty() {
        return None;
    }
    let Some((account_key, owner_id, manual_bal)) = extract_account_change_fields(payload_args)
    else {
        return None;
    };
    let Some(bridge) = lookup_commit_bridge(ctx) else {
        return None;
    };
    let staging_scope_id =
        current_dispatch_staging_scope_id(Some(&*ctx.pending_notifications), Some(&ctx.notify_tx));
    let matching: Vec<ProjectedAccountRecord> = {
        let ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
        let current_wallet_epoch = ledger.wallet_epoch(owner_id, &account_key);
        ledger
            .records
            .iter()
            .filter(|r| {
                r.staging_scope_id == staging_scope_id
                    && current_wallet_epoch.saturating_sub(r.commit_epoch)
                        <= MAX_RECENT_COMMIT_EPOCH_LAG
                    && r.account_key == account_key
                    && r.owner_id == owner_id
            })
            .cloned()
            .collect()
    };
    if matching.is_empty() {
        return None;
    }

    let mut queues = staging_scope_id.is_none().then(|| {
        ctx.pending_notifications
            .write()
            .unwrap_or_else(|e| e.into_inner())
    });
    let mut emitted = Vec::with_capacity(recipients.len());
    let mut seq_updates: Vec<(i64, u64, u64)> = Vec::new();

    if !recipients.is_empty() {
        #[cfg(any(test, feature = "testkit"))]
        crate::router::notify_registry::assert_registered_event_shape_if_registered(
            "OnAccountChange",
            payload_args,
        );
    }
    for &(cid, uid) in recipients {
        let make_notif = || {
            assemble_pending_notification(
                cid,
                uid,
                "OnAccountChange",
                id_type,
                payload_args.to_vec(),
                now_ft,
            )
        };
        let cid_records: Vec<&ProjectedAccountRecord> =
            matching.iter().filter(|r| r.char_id == cid).collect();
        let latest_rec_opt = cid_records.last().copied();
        let exact_idx_opt = cid_records
            .iter()
            .rposition(|r| (r.balance - manual_bal).abs() < 1e-6);
        match (latest_rec_opt, queues.as_mut()) {
            (Some(latest), Some(queues)) => {
                let queue = queues.entry(cid).or_default();
                let latest_pos = queue.iter().position(|n| n.seq_id == latest.seq_id);
                if let Some(exact_idx) = exact_idx_opt {
                    let exact = cid_records[exact_idx];
                    if let Some(pos) = queue.iter().position(|n| n.seq_id == exact.seq_id) {
                        emitted.push(queue[pos].clone());
                        continue;
                    }
                    // `exact` was already drained. Keep suppressing unless a newer wallet
                    // commit intervened before a still-queued `latest` (cross-epoch round-trip).
                    let round_tripped_alias =
                        exact_idx + 1 < cid_records.len() - 1 && latest_pos.is_some();
                    if !round_tripped_alias {
                        emitted.push(make_notif());
                        continue;
                    }
                }
                if let Some(pos) = latest_pos {
                    let mut notif = make_notif();
                    let new_tail_seq = next_notification_seq_id();
                    notif.seq_id = latest.seq_id;
                    queue[pos].seq_id = new_tail_seq;
                    insert_before_pos_with_cap(queue, pos, notif.clone());
                    seq_updates.push((cid, latest.seq_id, new_tail_seq));
                    // Do not re-broadcast stale pre-settlement balance on `notify_tx`
                    // after `latest.seq_id` (`B_final`) was already sent at commit time.
                    emitted.push(notif);
                } else {
                    // `latest.seq_id` was already drained to live listeners: do not re-push
                    // duplicates or stale intermediate balances onto the live stream.
                    emitted.push(make_notif());
                }
            }
            (None, Some(queues)) => {
                let notif = make_notif();
                push_pending_notification_capped(queues.entry(cid).or_default(), notif.clone());
                let _ = ctx.notify_tx.send((cid, notif.clone()));
                emitted.push(notif);
            }
            (Some(latest), None) => {
                if let Some(existing) = cid_records.iter().rev().find_map(|r| {
                    if (r.balance - manual_bal).abs() < 1e-6 {
                        find_staged_notification_by_seq(&*ctx.pending_notifications, r.seq_id)
                    } else {
                        None
                    }
                }) {
                    emitted.push(existing);
                } else if exact_idx_opt.is_some() {
                    // Exact balance for this staged scope was already flushed or drained.
                    emitted.push(make_notif());
                } else if find_staged_notification_by_seq(&*ctx.pending_notifications, latest.seq_id)
                    .is_some()
                {
                    let mut notif = make_notif();
                    if let Some(new_tail_seq) = insert_before_staged_seq_or_stage(
                        &*ctx.pending_notifications,
                        &ctx.notify_tx,
                        latest.seq_id,
                        &mut notif,
                    ) {
                        seq_updates.push((cid, latest.seq_id, new_tail_seq));
                    }
                    emitted.push(notif);
                } else {
                    emitted.push(make_notif());
                }
            }
            (None, None) => {
                let notif = make_notif();
                let _ = try_stage_dispatch_notification(
                    Some(&*ctx.pending_notifications),
                    Some(&ctx.notify_tx),
                    &notif,
                    None,
                );
                emitted.push(notif);
            }
        }
    }
    drop(queues);
    if !seq_updates.is_empty() {
        let mut ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
        for (cid, old_seq, new_seq) in seq_updates {
            if let Some(rec) = ledger
                .records
                .iter_mut()
                .rev()
                .find(|r| r.char_id == cid && r.seq_id == old_seq)
            {
                rec.seq_id = new_seq;
            }
        }
    }

    Some(emitted)
}

impl ServerContext {
    /// Emits an `OnAccountChange` notification (`accountKey`, `ownerID`, `balance`).
    #[track_caller]
    pub fn notify_account_change(
        &self,
        char_id: i64,
        account_key: &str,
        owner_id: i64,
        new_balance_isk: f64,
    ) -> Vec<PendingMachoNotification> {
        self.emit_typed_with_id_type(
            NotificationTarget::Character(char_id),
            "charid",
            OnAccountChange {
                account_key: account_key.into(),
                owner_id,
                balance: new_balance_isk,
            },
        )
    }

    /// Emits `OnAccountChange` for personal (`cash`) or corporate division (`cash` / `cash2..cash7`) wallets.
    #[track_caller]
    pub fn notify_wallet_balance(
        &self,
        is_corp: bool,
        owner_id: i64,
        div_id: u8,
        balance: eve_db::IskCents,
    ) -> Vec<PendingMachoNotification> {
        let (target, id_type, key) = if is_corp {
            let key = if div_id <= 1 {
                "cash".to_string()
            } else {
                format!("cash{div_id}")
            };
            (NotificationTarget::Corporation(owner_id), "corpid", key)
        } else {
            (
                NotificationTarget::Character(owner_id),
                "charid",
                "cash".to_string(),
            )
        };
        self.emit_typed_with_id_type(
            target,
            id_type,
            OnAccountChange {
                account_key: key,
                owner_id,
                balance: balance.to_wire_f64(),
            },
        )
    }

    pub(crate) fn observe_active_fuel_emit(
        &self,
        recipients: &[(i64, Option<u64>)],
        event: &str,
        payload_args: &[PyRep],
    ) {
        if recipients.is_empty() {
            return;
        }
        self.active_fuel_reserves.observe_pre_emit_event(
            crate::services::tactical_modules::db_ptr_key(&self.db),
            event,
            payload_args,
        );
    }

    pub(crate) fn should_emit_item_on_exact_rollback(
        &self,
        reserve_guard: Option<&crate::services::tactical_modules::ActiveFuelReserveGuard>,
        item_id: i64,
    ) -> bool {
        reserve_guard
            .is_some_and(|guard| self.active_fuel_reserves.has_exposed_since(guard, item_id))
    }

    /// Restores previously reserved stack quantities inside an atomic `WorldRepository::tx`
    /// and queues `#931` `TxNotificationEffect`s (`OnItemChange` / `OnBlueprintsUpdated`) for any
    /// diverged, orphaned, or externally-exposed stacks (`#1486`, `#931`).
    pub fn rollback_consumed_items_tx(
        &self,
        fallback_char_id: i64,
        reserved: &[(eve_db::InventoryItemRecord, i32)],
        reserve_guard: Option<&crate::services::tactical_modules::ActiveFuelReserveGuard>,
    ) {
        if reserved.is_empty() {
            return;
        }
        let _ = self.db.tx::<(), eve_db::TxError>("rollback_consumed_items", |wtx| {
            let diffs = wtx.snap.rollback_restore_consumed_stacks_if_exposed(
                reserved,
                |pre, _, _| self.should_emit_item_on_exact_rollback(reserve_guard, pre.item_id),
            );
            super::delta_projection::push_item_diffs_tx_effects(
                wtx.snap,
                wtx.effects,
                &self.sde,
                fallback_char_id,
                &diffs,
            );
            Ok(())
        });
    }

    /// Emits deferred `#931` item diff effects for reserved fuel after validation succeeds,
    /// keeping `_fuel_guard` alive across the atomic `WorldRepository::tx` (`#1486`, `BF-931-3C1-01`).
    pub(crate) fn emit_reserved_fuel_item_diffs(
        &self,
        tx_label: &'static str,
        char_id: i64,
        item_diffs: &[(eve_db::InventoryItemRecord, Option<eve_db::InventoryItemRecord>)],
        _fuel_guard: crate::services::tactical_modules::ActiveFuelReserveGuard,
    ) {
        if item_diffs.is_empty() {
            return;
        }
        // Run live read + `#931` effect emission atomically inside `WorldRepository::tx` so
        // snapshot observation, `observe_pre_emit_event`, and notification staging serialize
        // under `WorldRepository::writer` with `rollback_consumed_items_tx` and concurrent item mutations.
        let _ = self.db.tx::<(), eve_db::TxError>(tx_label, |wtx| {
            let mut live_diffs = Vec::with_capacity(item_diffs.len());
            for (pre, captured_post) in item_diffs {
                let current = wtx
                    .snap
                    .items
                    .get(&pre.item_id)
                    .filter(|cur| cur.owner_id == pre.owner_id)
                    .cloned();
                let adjusted_pre;
                let old_ref = match current.as_ref() {
                    Some(cur)
                        if captured_post.as_ref() != Some(cur)
                            && super::delta_projection::compute_item_change_entries(pre, cur)
                                .is_empty() =>
                    {
                        adjusted_pre = eve_db::InventoryItemRecord {
                            quantity: if cur.quantity < i32::MAX {
                                cur.quantity + 1
                            } else {
                                cur.quantity - 1
                            },
                            ..pre.clone()
                        };
                        adjusted_pre
                    }
                    _ => pre.clone(),
                };
                live_diffs.push((Some(old_ref), current));
            }
            super::delta_projection::push_item_diffs_tx_effects(
                wtx.snap,
                wtx.effects,
                &self.sde,
                char_id,
                &live_diffs,
            );
            Ok(())
        });
    }

    /// Emits a virtual V24 `OnItemChange` (`location = 10`, `flag = 147`, old `(locationID, flagID)`)
    /// directly via transaction effects when a fitted module or rig is destroyed so `invCache`
    /// fires `OnModuleUnfitted` before any sibling charge deletion tombstone (`#931` Guardrail 4).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_tx_fitted_item_trashed(
        effects: &mut Effects,
        sde: &SdeRegistry,
        char_id: i64,
        item_id: i64,
        type_id: i64,
        quantity: i64,
        old_location_id: i64,
        old_flag: i64,
    ) {
        if char_id <= 0 {
            return;
        }
        let args = build_fitted_item_trashed_args(
            sde,
            char_id,
            item_id,
            type_id,
            quantity,
            old_location_id,
            old_flag,
        );
        effects.notify_character(char_id, "OnItemChange", "charid", args);
    }
}

fn collect_touched_monitor_types(changes: &ChangeSet) -> HashMap<i64, HashSet<u32>> {
    let mut touched_types: HashMap<i64, HashSet<u32>> = HashMap::new();
    for change in &changes.items {
        for rec in [change.before.as_deref(), change.after.as_deref()]
            .into_iter()
            .flatten()
        {
            touched_types
                .entry(rec.owner_id)
                .or_default()
                .insert(rec.type_id);
        }
    }
    touched_types
}

fn prune_monitor_order(monitors: &CoveredMonitorMap, order: Option<&mut VecDeque<i64>>) {
    if let Some(q) = order {
        q.retain(|id| monitors.contains_key(id));
    }
}

fn invalidate_covered_monitors_for_types(
    monitors: &mut CoveredMonitorMap,
    order: Option<&mut VecDeque<i64>>,
    touched_types: &HashMap<i64, HashSet<u32>>,
) {
    if monitors.is_empty() || touched_types.is_empty() {
        return;
    }
    monitors.retain(|_, (_, oid, tids)| {
        if let Some(removed) = touched_types.get(oid) {
            tids.retain(|tid| !removed.contains(tid));
        }
        !tids.is_empty()
    });
    prune_monitor_order(monitors, order);
}

fn retain_monitors_excluding_chars(
    monitors: &mut CoveredMonitorMap,
    order: Option<&mut VecDeque<i64>>,
    chars: &[i64],
) {
    if !chars.is_empty() && !monitors.is_empty() {
        monitors.retain(|_, (cid, oid, _)| !chars.contains(cid) && !chars.contains(oid));
        prune_monitor_order(monitors, order);
    }
}

fn merge_covered_industry_monitors(
    target: &mut CoveredMonitorMap,
    mut order: Option<&mut VecDeque<i64>>,
    max_entries: Option<usize>,
    covered: &HashMap<i64, (i64, i64, Vec<u32>)>,
    excluded_chars: &[i64],
) {
    for (&monitor_id, (char_id, owner_id, tids)) in covered {
        if tids.is_empty()
            || excluded_chars.contains(char_id)
            || excluded_chars.contains(owner_id)
        {
            continue;
        }
        let was_present = target.contains_key(&monitor_id);
        let entry = target
            .entry(monitor_id)
            .or_insert_with(|| (*char_id, *owner_id, HashSet::with_capacity(tids.len())));
        if entry.0 != *char_id || entry.1 != *owner_id {
            entry.0 = *char_id;
            entry.1 = *owner_id;
            entry.2.clear();
        }
        entry.2.extend(tids.iter().copied());
        if let Some(q) = order.as_deref_mut() {
            if was_present {
                q.retain(|&id| id != monitor_id);
            }
            q.push_back(monitor_id);
        }
    }
    if let (Some(q), Some(cap)) = (order, max_entries) {
        while target.len() > cap {
            let Some(oldest_id) = q.pop_front() else {
                break;
            };
            target.remove(&oldest_id);
        }
    }
}

pub(crate) fn is_industry_monitor_covered(
    ctx: &ServerContext,
    monitor_id: i64,
    tids: &[u32],
) -> bool {
    if tids.is_empty() {
        return false;
    }
    let scope_id = current_dispatch_staging_scope_id(
        Some(&*ctx.pending_notifications),
        Some(&ctx.notify_tx),
    );
    let Some(bridge) = lookup_commit_bridge(ctx) else {
        return false;
    };
    let mut ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
    ledger.is_monitor_covered(scope_id, monitor_id, tids)
}

pub(crate) fn clear_covered_monitors_for_char(ctx: &ServerContext, char_id: i64) {
    let Some(bridge) = lookup_commit_bridge(ctx) else {
        return;
    };
    let mut ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
    ledger.clear_monitors_for_chars(&[char_id]);
}

pub(crate) fn clear_covered_monitor_by_id(ctx: &ServerContext, monitor_id: i64) {
    let Some(bridge) = lookup_commit_bridge(ctx) else {
        return;
    };
    let mut ledger = bridge.recent_epoch.lock().unwrap_or_else(|e| e.into_inner());
    ledger.remove_monitor_by_id(monitor_id);
}

/// Builds the V24 `OnItemChange` payload (`location = 10`, `flag = 147`) expected for fitted
/// module/rig destruction so `invCache` fires `OnModuleUnfitted` (`#931` Guardrail 4).
pub(crate) fn build_fitted_item_trashed_args(
    sde: &SdeRegistry,
    char_id: i64,
    item_id: i64,
    type_id: i64,
    quantity: i64,
    old_location_id: i64,
    old_flag: i64,
) -> Vec<PyRep> {
    let (group_id, category_id) = u32::try_from(type_id)
        .ok()
        .and_then(|tid| sde.types.get(&tid))
        .map(|t| (i64::from(t.group_id), i64::from(t.category_id)))
        .unwrap_or((0, 0));
    // Fitted modules/rigs are singleton items, so their packed inv_item quantity is -1 when qty == 1.
    let packed_qty = if quantity == 1 { -1 } else { quantity };
    let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(item_id),
        PyRep::Integer(type_id),
        PyRep::Integer(char_id),
        PyRep::Integer(10),
        PyRep::Integer(147),
        PyRep::Integer(packed_qty),
        PyRep::Integer(group_id),
        PyRep::Integer(category_id),
        PyRep::String(String::new()),
    ]);
    vec![
        row,
        PyRep::Dict(vec![
            (PyRep::Integer(3), PyRep::Integer(old_location_id)),
            (PyRep::Integer(4), PyRep::Integer(old_flag)),
        ]),
    ]
}

#[cfg(test)]
mod t3c2_tests {
    use super::super::cache_notifications::{
        flush_staged_dispatch_notifications, with_staged_dispatch_notifications,
    };
    use super::ServerContext;
    use eve_db::IskCents;

    #[test]
    fn commit_bridges_are_isolated_per_repository_and_freed_on_drop() {
        let pair = eve_testkit::process_global_ratchet::TwoContextPair::from_factory(
            ServerContext::new,
        );
        let left = pair.left();
        let right = pair.right();

        let left_bridge = super::commit_bridge_state(left).expect("left bridge installed");
        let right_bridge = super::commit_bridge_state(right).expect("right bridge installed");
        assert!(!std::sync::Arc::ptr_eq(&left_bridge, &right_bridge));

        let left_db_bridges = super::commit_bridges_for_db(&left.db);
        let right_db_bridges = super::commit_bridges_for_db(&right.db);
        assert_eq!(left_db_bridges.len(), 1);
        assert_eq!(right_db_bridges.len(), 1);
        assert!(std::sync::Arc::ptr_eq(&left_db_bridges[0], &left_bridge));
        assert!(std::sync::Arc::ptr_eq(&right_db_bridges[0], &right_bridge));

        // Verify shared-repo contexts keep distinct per-ServerContext bridges without colliding,
        // and dropping an independent ServerContext + WorldRepository frees both the slot and bridge.
        let standalone = ServerContext::new();
        let standalone_bridge =
            super::commit_bridge_state(&standalone).expect("standalone bridge installed");
        let weak_bridge = std::sync::Arc::downgrade(&standalone_bridge);
        let weak_slot = standalone
            .db
            .get_transient_runtime_slot::<super::CommitBridgesRepoSlot>(
                super::COMMIT_BRIDGES_SLOT_KEY,
            )
            .flatten()
            .map(|slot| std::sync::Arc::downgrade(&slot))
            .expect("commit bridges slot initialized");
        let shared_db = std::sync::Arc::clone(&left.db);
        let shared_ctx = ServerContext::with_sde_and_repository(
            std::sync::Arc::clone(&left.sde),
            shared_db,
        );
        let shared_ctx_bridge =
            super::commit_bridge_state(&shared_ctx).expect("shared_ctx bridge installed");
        assert!(!std::sync::Arc::ptr_eq(&shared_ctx_bridge, &left_bridge));
        assert_eq!(super::commit_bridges_for_db(&left.db).len(), 2);

        drop(shared_ctx_bridge);
        drop(shared_ctx);
        drop(standalone_bridge);
        drop(standalone);
        assert!(
            weak_slot.upgrade().is_none(),
            "dropping standalone WorldRepository must drop CommitBridgesRepoSlot"
        );
        assert!(
            weak_bridge.upgrade().is_none(),
            "dropping standalone WorldRepository must drop DeltaCommitBridgeState"
        );
    }

    #[tokio::test]
    async fn account_change_helper_dedupes_matching_and_passes_through_unmatched() {
        let ctx = ServerContext::new();
        let (pilot_id, other_id) = (90_000_001_i64, 90_000_099_i64);
        let uid = ctx.db.with_snapshot(|s| s.characters[&pilot_id].user_id);
        ctx.db.acquire_online_lock(uid, 93_203, Some(pilot_id), true, 1_700_000_500).expect("lock");
        let _ = ctx.drain_notifications_for_char(pilot_id);

        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            let new_bal = ctx
                .db
                .tx("wallet_delta_t3c2", |tx| -> Result<f64, eve_db::TxError> {
                    let ch = tx.snap.characters.get_mut(&pilot_id).expect("pilot");
                    ch.balance = IskCents(ch.balance.0 + 1_000_000);
                    Ok(ch.balance.to_f64_isk())
                })
                .expect("tx");
            // Matching account delta token is deduplicated; non-matching owner passes through.
            ctx.notify_account_change(pilot_id, "cash", pilot_id, new_bal);
            ctx.notify_account_change(pilot_id, "cash", other_id, new_bal);
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;

        let notifs: Vec<_> = ctx
            .drain_notifications_for_char(pilot_id)
            .into_iter()
            .filter(|n| n.event == "OnAccountChange")
            .collect();
        assert_eq!(notifs.len(), 2, "expected 1 deduped pilot + 1 other OnAccountChange, got {notifs:?}");
    }

    fn setup_monitored_char_and_tritanium(
        ctx: &ServerContext,
        char_id: i64,
        station_id: i64,
        monitor_ids: &[i64],
        trit_item_id: i64,
    ) -> u64 {
        let uid = ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters.get(&char_id).cloned().expect("char");
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            let uid = ch.user_id;
            snap.characters.insert(char_id, ch);
            snap.items.retain(|_, it| {
                !(it.owner_id == char_id
                    && it.location_id == station_id
                    && it.flag == eve_db::InventoryFlag::Hangar)
            });
            snap.items.insert(
                trit_item_id,
                eve_db::InventoryItemRecord {
                    item_id: trit_item_id,
                    type_id: 34,
                    owner_id: char_id,
                    location_id: station_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: 500,
                    singleton: false,
                    ..Default::default()
                },
            );
            let mut monitors = std::collections::BTreeMap::new();
            for &mon_id in monitor_ids {
                monitors.insert(
                    mon_id,
                    crate::services::notify_emitters_character_services::IndustryMaterialMonitorSub {
                        char_id,
                        owner_id: char_id,
                        location_id: station_id,
                        flag_id: eve_db::InventoryFlag::Hangar as i64,
                    },
                );
            }
            crate::services::notify_emitters_character_services::save_industry_material_monitors(
                snap,
                &monitors,
            );
            uid
        });
        ctx.db
            .acquire_online_lock(
                uid,
                93_300 + (char_id as u64 % 1000),
                Some(char_id),
                true,
                1_700_000_500,
            )
            .expect("online lock");
        let _ = ctx.drain_notifications_for_char(char_id);
        uid
    }

    fn suppressed_on_item_change_args(
        char_id: i64,
        item_id: i64,
        station_id: i64,
        new_qty: i64,
        old_qty: i64,
    ) -> Vec<super::PyRep> {
        let row = eve_proto::DBRowDescriptor::inv_item_header().build_dbrow(vec![
            super::PyRep::Integer(item_id),
            super::PyRep::Integer(34),
            super::PyRep::Integer(char_id),
            super::PyRep::Integer(station_id),
            super::PyRep::Integer(eve_db::InventoryFlag::Hangar as i64),
            super::PyRep::Integer(new_qty),
            super::PyRep::Integer(18),
            super::PyRep::Integer(4),
            super::PyRep::String(String::new()),
        ]);
        vec![
            row,
            super::PyRep::Dict(vec![(super::PyRep::Integer(9), super::PyRep::Integer(old_qty))]),
        ]
    }

    #[test]
    fn outside_dispatch_suppressed_commit_invalidates_covered_industry_monitor() {
        let ctx = ServerContext::new();
        let (char_id, station_id, mon_id, trit_id) =
            (90_000_001_i64, 60_003_760_i64, 88_901_i64, 995_931_201_i64);
        setup_monitored_char_and_tritanium(&ctx, char_id, station_id, &[mon_id], trit_id);

        ctx.db
            .tx("outside_cover", |tx| {
                tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 400;
                Ok::<_, eve_db::TxError>(())
            })
            .expect("outside_cover");
        assert!(super::is_industry_monitor_covered(&ctx, mon_id, &[34]));
        let _ = ctx.drain_notifications_for_char(char_id);

        // Subsequent commit touching (char_id, 34) under tx.effects suppression does not re-cover
        // the monitor and must invalidate prior outside-dispatch coverage so follow-up emit fires.
        ctx.db
            .tx("outside_suppressed", |tx| {
                tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 150;
                tx.effects.notify_character(
                    char_id,
                    "OnItemChange",
                    "charid",
                    suppressed_on_item_change_args(char_id, trit_id, station_id, 150, 400),
                );
                Ok::<_, eve_db::TxError>(())
            })
            .expect("outside_suppressed");
        assert!(!super::is_industry_monitor_covered(&ctx, mon_id, &[34]));
        let _ = ctx.drain_notifications_for_char(char_id);

        crate::services::notify_emitters_character_services::emit_industry_materials_consumed(
            &ctx,
            char_id,
            mon_id,
            station_id,
            station_id,
            char_id,
            false,
            eve_db::InventoryFlag::Hangar,
            None,
            &[(34, 250)],
        );
        let mats: Vec<_> = ctx
            .drain_notifications_for_char(char_id)
            .into_iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .collect();
        assert_eq!(mats.len(), 1, "follow-up OnIndustryMaterials must emit after invalidation");
        assert_eq!(mats[0].args[0], super::PyRep::Integer(mon_id));
        assert_eq!(
            mats[0].args[1],
            super::PyRep::Dict(vec![(super::PyRep::Integer(34), super::PyRep::Integer(150))])
        );
    }

    #[tokio::test]
    async fn dispatch_suppressed_commit_and_transition_invalidate_covered_monitors() {
        let ctx = ServerContext::new();
        let (char_id, station_id, mon_id, trit_id) =
            (90_000_001_i64, 60_003_760_i64, 88_902_i64, 995_931_202_i64);
        setup_monitored_char_and_tritanium(&ctx, char_id, station_id, &[mon_id], trit_id);

        // 1. Multi-commit dispatch: Commit 1 covers monitor; suppressed Commit 2 invalidates it so
        //    follow-up emit_industry_materials_consumed fires cleanly inside the same dispatch.
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("dispatch_cover", |tx| {
                    tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 400;
                    Ok::<_, eve_db::TxError>(())
                })
                .expect("dispatch_cover");
            assert!(super::is_industry_monitor_covered(&ctx, mon_id, &[34]));

            ctx.db
                .tx("dispatch_suppressed", |tx| {
                    tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 125;
                    tx.effects.notify_character(
                        char_id,
                        "OnItemChange",
                        "charid",
                        suppressed_on_item_change_args(char_id, trit_id, station_id, 125, 400),
                    );
                    Ok::<_, eve_db::TxError>(())
                })
                .expect("dispatch_suppressed");
            assert!(!super::is_industry_monitor_covered(&ctx, mon_id, &[34]));

            crate::services::notify_emitters_character_services::emit_industry_materials_consumed(
                &ctx,
                char_id,
                mon_id,
                station_id,
                station_id,
                char_id,
                false,
                eve_db::InventoryFlag::Hangar,
                None,
                &[(34, 275)],
            );
            flush_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx);
        })
        .await;

        let mats: Vec<_> = ctx
            .drain_notifications_for_char(char_id)
            .into_iter()
            .filter(|n| n.event == "OnIndustryMaterials")
            .collect();
        assert_eq!(mats.len(), 2, "expected Commit 1 + post-invalidation follow-up OnIndustryMaterials, got {mats:?}");
        assert_eq!(
            mats[1].args[1],
            super::PyRep::Dict(vec![(super::PyRep::Integer(34), super::PyRep::Integer(125))])
        );

        // 2. Multi-commit dispatch: Commit 2 transitions subscriber character -> purges monitor coverage.
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("dispatch_cover_before_transition", |tx| {
                    tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 100;
                    Ok::<_, eve_db::TxError>(())
                })
                .expect("dispatch_cover_before_transition");
            assert!(super::is_industry_monitor_covered(&ctx, mon_id, &[34]));

            ctx.db
                .tx("dispatch_undock_transition", |tx| {
                    tx.snap.characters.get_mut(&char_id).expect("char").station_id = None;
                    Ok::<_, eve_db::TxError>(())
                })
                .expect("dispatch_undock_transition");
            assert!(!super::is_industry_monitor_covered(&ctx, mon_id, &[34]));
        })
        .await;
    }

    #[tokio::test]
    async fn disconnect_job_and_clear_notifications_prune_covered_monitors() {
        let ctx = ServerContext::new();
        let (char_id, station_id, mon_a, mon_b, trit_id) =
            (90_000_001_i64, 60_003_760_i64, 88_903_i64, 88_904_i64, 995_931_203_i64);
        let uid = setup_monitored_char_and_tritanium(
            &ctx,
            char_id,
            station_id,
            &[mon_a, mon_b],
            trit_id,
        );
        let mut sess = eve_machonet::SessionState {
            user_id: Some(uid),
            character_id: Some(char_id),
            corporation_id: Some(1_000_009),
            solar_system_id: Some(30_000_142),
            station_id: Some(station_id),
            ..Default::default()
        };

        // Outside-dispatch DisconnectJob(mon_a) removes only mon_a; DisconnectJob(None) clears mon_b.
        ctx.db
            .tx("cover_both_outside", |tx| {
                tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 420;
                Ok::<_, eve_db::TxError>(())
            })
            .expect("cover_both_outside");
        assert!(super::is_industry_monitor_covered(&ctx, mon_a, &[34]));
        assert!(super::is_industry_monitor_covered(&ctx, mon_b, &[34]));

        let disc_a = crate::services::industry_pi::handle_industry_job_queries(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[super::PyRep::Integer(mon_a)],
            &std::collections::HashMap::new(),
        )
        .expect("handled")
        .expect("DisconnectJob(mon_a) ok");
        assert_eq!(disc_a, super::PyRep::Bool(true));
        assert!(!super::is_industry_monitor_covered(&ctx, mon_a, &[34]));
        assert!(super::is_industry_monitor_covered(&ctx, mon_b, &[34]));

        let disc_all = crate::services::industry_pi::handle_industry_job_queries(
            &ctx,
            &mut sess,
            "industryMonitor",
            "DisconnectJob",
            &[super::PyRep::None],
            &std::collections::HashMap::new(),
        )
        .expect("handled")
        .expect("DisconnectJob(None) ok");
        assert_eq!(disc_all, super::PyRep::None);
        assert!(!super::is_industry_monitor_covered(&ctx, mon_b, &[34]));

        // Re-register mon_a, cover inside dispatch, and verify clear_notifications_for_char clears it.
        ctx.db.with_mut_snapshot(|snap| {
            let mut monitors = std::collections::BTreeMap::new();
            monitors.insert(
                mon_a,
                crate::services::notify_emitters_character_services::IndustryMaterialMonitorSub {
                    char_id,
                    owner_id: char_id,
                    location_id: station_id,
                    flag_id: eve_db::InventoryFlag::Hangar as i64,
                },
            );
            crate::services::notify_emitters_character_services::save_industry_material_monitors(
                snap,
                &monitors,
            );
        });
        with_staged_dispatch_notifications(&*ctx.pending_notifications, &ctx.notify_tx, async {
            ctx.db
                .tx("cover_in_dispatch_clear", |tx| {
                    tx.snap.items.get_mut(&trit_id).expect("trit").quantity = 350;
                    Ok::<_, eve_db::TxError>(())
                })
                .expect("cover_in_dispatch_clear");
            assert!(super::is_industry_monitor_covered(&ctx, mon_a, &[34]));
            ctx.clear_notifications_for_char(char_id);
            assert!(!super::is_industry_monitor_covered(&ctx, mon_a, &[34]));
        })
        .await;
    }

    #[test]
    fn epoch_ledger_monitors_enforce_identity_reset_and_bounded_eviction() {
        let mut ledger = super::EpochLedger::default();
        let covered_a: std::collections::HashMap<i64, (i64, i64, Vec<u32>)> =
            [(777_i64, (90_000_001_i64, 90_000_001_i64, vec![34]))]
                .into_iter()
                .collect();
        ledger.record_outside_covered_monitors(&covered_a, &[]);
        assert!(ledger.is_monitor_covered(None, 777, &[34]));

        // Identity change on reused monitor_id must reset previous type set.
        let covered_b: std::collections::HashMap<i64, (i64, i64, Vec<u32>)> =
            [(777_i64, (90_000_002_i64, 98_000_001_i64, vec![35]))]
                .into_iter()
                .collect();
        ledger.record_outside_covered_monitors(&covered_b, &[]);
        assert!(!ledger.is_monitor_covered(None, 777, &[34]));
        assert!(ledger.is_monitor_covered(None, 777, &[35]));

        // Clearing by owner_id removes corp/owner-matched monitors even when sub_char_id differs.
        ledger.clear_monitors_for_chars(&[98_000_001_i64]);
        assert!(!ledger.is_monitor_covered(None, 777, &[35]));
        ledger.record_outside_covered_monitors(&covered_b, &[]);
        assert!(ledger.is_monitor_covered(None, 777, &[35]));

        // Bounded eviction caps outside_covered_monitors at MAX_OUTSIDE_COVERED_MONITORS.
        let overflow = 16usize;
        for offset in 0..(super::MAX_OUTSIDE_COVERED_MONITORS + overflow) {
            let mon_id = 10_000 + offset as i64;
            let batch: std::collections::HashMap<i64, (i64, i64, Vec<u32>)> =
                [(mon_id, (90_000_001, 90_000_001, vec![34]))]
                    .into_iter()
                    .collect();
            ledger.record_outside_covered_monitors(&batch, &[]);
        }
        assert_eq!(ledger.outside_covered_monitors.len(), super::MAX_OUTSIDE_COVERED_MONITORS);
        assert_eq!(ledger.outside_covered_monitor_order.len(), super::MAX_OUTSIDE_COVERED_MONITORS);
        assert!(!ledger.is_monitor_covered(None, 777, &[35]));
        assert!(!ledger.is_monitor_covered(None, 10_000, &[34]));
        assert!(ledger.is_monitor_covered(
            None,
            10_000 + (super::MAX_OUTSIDE_COVERED_MONITORS + overflow - 1) as i64,
            &[34],
        ));
    }
}
