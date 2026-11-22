//! Commit-time wallet/PLEX delivery & explicit in-transaction effect bridge (`#931` Tracks 1–2).
//!
//! Wires [`DeltaStateCache`] + [`DefaultDeltaProjection::wallets_only`] into
//! `WorldRepository::register_post_commit_listener` so committed `ChangeSet` deltas on
//! personal ISK (`"cash"`), corp divisions `1..=7` (`"cash"` / `"cash2".."cash7"`), and PLEX
//! (`"plex"`) materialize `OnAccountChange` notifications, while forwarding committed item
//! changes to [`delta_item_compare::ItemCompareBridge`].

#![forbid(unsafe_code)]

use crate::router::cache_notifications::{
    assemble_pending_notification, current_dispatch_staging_scope_info,
    enqueue_or_stage_pending_batch, find_staged_notification_by_seq,
    insert_before_staged_seq_or_stage, is_active_dispatch_char, next_notification_seq_id,
    pending_notifications_ptr, push_pending_notification_capped, try_stage_dispatch_notification,
    NotificationTarget, PendingMachoNotification, MAX_ACTIVE_RPC_NOTIFICATIONS_PER_CHAR,
    MAX_PENDING_NOTIFICATIONS_PER_CHAR,
};
use crate::router::context::ServerContext;
use crate::router::delta_item_compare::{ItemCompareBridge, TxItemEffectKeys};
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
use std::sync::{Arc, Mutex, OnceLock, Weak};

type BridgeKey = (u64, usize);
const MAX_RECENT_COMMIT_EPOCH_LAG: u64 = 32;
const MAX_RECENT_PROJECTED_RECORDS: usize = 4096;

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
}

struct DeltaCommitBridgeState {
    recent_epoch: Mutex<EpochLedger>,
    item_compare: Arc<ItemCompareBridge>,
}

static COMMIT_BRIDGES: OnceLock<Mutex<HashMap<BridgeKey, Weak<DeltaCommitBridgeState>>>> =
    OnceLock::new();

fn commit_bridges() -> &'static Mutex<HashMap<BridgeKey, Weak<DeltaCommitBridgeState>>> {
    COMMIT_BRIDGES.get_or_init(|| Mutex::new(HashMap::new()))
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
) -> (EmittedEffectKeys, Vec<ProjectedAccountRecord>) {
    let mut emitted_effects = EmittedEffectKeys::default();
    let mut records = Vec::new();
    let mut built = Vec::new();

    for effect in &effects.notifications {
        let recipients = resolve_tx_effect_recipients(db, snap, effect);
        if recipients.is_empty() {
            continue;
        }
        let is_account_change = effect.event == "OnAccountChange";
        let fields_opt = if is_account_change {
            extract_account_change_fields(&effect.args)
        } else {
            None
        };
        let suppression_key = fields_opt
            .as_ref()
            .map(|(k, owner_id, _)| (k.clone(), *owner_id));
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
/// `TxNotificationEffect`s, projecting committed wallet / corp-division / PLEX Vault deltas
/// (`OnAccountChange`), and wiring compare-only item/blueprint parity telemetry (`#931` Tracks 1–2).
pub(crate) fn install_tx_post_commit_listener(ctx: &ServerContext) {
    let pend_ptr = pending_notifications_ptr(&*ctx.pending_notifications);
    let bridge_key = (ctx.db.instance_id(), pend_ptr);
    let bridge = {
        let mut map = commit_bridges().lock().unwrap_or_else(|e| e.into_inner());
        map.retain(|_, w| w.strong_count() > 0);
        if map.get(&bridge_key).and_then(Weak::upgrade).is_some() {
            return;
        }
        let state = Arc::new(DeltaCommitBridgeState {
            recent_epoch: Mutex::new(EpochLedger::default()),
            item_compare: Arc::new(ItemCompareBridge::new_with_switches(
                ctx.db.instance_id(),
                Arc::downgrade(&ctx.projected_item_switches),
            )),
        });
        map.insert(bridge_key, Arc::downgrade(&state));
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
    let switches = Arc::clone(&ctx.projected_item_switches);

    ctx.db.register_post_commit_listener(Arc::new(
        move |snap, changes, effects| {
            let (Some(db), Some(pend)) = (db_w.upgrade(), pend_w.upgrade()) else {
                return;
            };
            let now_ft = db.clock().now_blueos_filetime_ticks();
            let now_unix = db.now_unix_secs();
            // Assigned per `(owner_id, account_key)` inside `EpochLedger::record_batch`.
            let commit_epoch = 0u64;
            let staging_scope_info =
                current_dispatch_staging_scope_info(Some(&*pend), Some(&notify_tx));
            let staging_scope_id = staging_scope_info.as_ref().map(|(scope_id, _)| *scope_id);

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

            let mut switched_projected_keys = HashSet::new();
            if !changes.items.is_empty() || !changes.characters.is_empty() {
                let specs = crate::router::delta_item_switch::collect_switched_char_same_scene_item_specs(
                    &switches,
                    &*pend,
                    &notify_tx,
                    staging_scope_info.as_ref(),
                    &*db,
                    snap,
                    changes,
                    &sde,
                    &emitted_effects.item_effects,
                );
                if !specs.is_empty() {
                    let mut item_notifs = Vec::with_capacity(specs.len());
                    for spec in specs {
                        switched_projected_keys.insert((spec.recipient_char_id, spec.item_id));
                        item_notifs.push(assemble_pending_notification(
                            spec.recipient_char_id,
                            Some(spec.recipient_user_id),
                            "OnItemChange",
                            "charid",
                            spec.args,
                            now_ft,
                        ));
                    }
                    enqueue_or_stage_pending_batch(&*pend, &notify_tx, &item_notifs);
                }
            }

            bridge.item_compare.on_commit_with_switched(
                snap,
                changes,
                &sde,
                now_unix,
                &*db,
                &emitted_effects.item_effects,
                staging_scope_info,
                &switched_projected_keys,
            );

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
    let bridge_key = (
        ctx.db.instance_id(),
        pending_notifications_ptr(&*ctx.pending_notifications),
    );
    commit_bridges()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&bridge_key)
        .and_then(Weak::upgrade)
}

pub(crate) fn item_compare_bridge(ctx: &ServerContext) -> Option<Arc<ItemCompareBridge>> {
    lookup_commit_bridge(ctx).map(|b| Arc::clone(&b.item_compare))
}

pub(crate) fn item_compare_bridges_for_db(db_instance_id: u64) -> Vec<Arc<ItemCompareBridge>> {
    commit_bridges().lock().unwrap_or_else(|e| e.into_inner()).iter()
        .filter(|(&(id, _), _)| id == db_instance_id)
        .filter_map(|(_, w)| w.upgrade().map(|b| Arc::clone(&b.item_compare)))
        .collect()
}

/// Coordinates remaining unlocked manual `OnAccountChange` calls with same-epoch commit-projected
/// `OnAccountChange` notifications: deduplicates identical committed balances and preserves
/// pre-settlement intermediate balances ahead of the committed final balance.
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
    let (account_key, owner_id, manual_bal) = extract_account_change_fields(payload_args)?;
    let bridge = lookup_commit_bridge(ctx)?;
    let staging_scope_id =
        current_dispatch_staging_scope_info(Some(&*ctx.pending_notifications), Some(&ctx.notify_tx))
            .map(|(scope_id, _)| scope_id);
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
}
