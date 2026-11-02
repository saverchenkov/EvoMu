//! Helper structures and catalog loaders for `operations_manager`.
//!
//! Provides SDE `res:/staticdata/surveys.fsdbinary` catalog parsing, operation definitions caching,
//! and survey eligibility checks.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use super::operations_manager::{CharacterOperationsRecord, push_operation_event, load_store_pub, save_store_pub};
pub use super::operations_survey_handlers::*;

/// SDE item reward entry decoded from `operations.fsdbinary` (`onActivationRewards` / `onCompletionRewards`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeOperationItemReward {
    pub type_id: u32,
    pub amount: u32,
    pub race_id: Option<u32>,
}

/// Operation, category, site, and task definitions + SDE rewards loaded from
/// `operationcategories.fsdbinary`, `operationtreestructures.fsdbinary`, `operations.fsdbinary`,
/// `operationsites.fsdbinary`, `achievementevents.fsdbinary`, and `operationtasks.fsdbinary`.
#[derive(Debug, Clone, Default)]
pub struct OperationDefinitions {
    pub sde_categories: HashSet<i64>,
    pub cat_isk_rewards: HashMap<i64, u64>,
    pub cat_operation_trees: HashMap<i64, Vec<i64>>,
    pub op_default_category: HashMap<i64, i64>,
    pub sde_operations: HashSet<i64>,
    pub op_tasks: HashMap<i64, Vec<i64>>,
    pub op_prerequisites: HashMap<i64, Vec<i64>>,
    pub op_tree_parents: HashMap<i64, Vec<i64>>,
    pub op_site_ids: HashMap<i64, i64>,
    pub task_to_op: HashMap<i64, i64>,
    pub op_isk_rewards: HashMap<i64, u64>,
    pub op_activation_rewards: HashMap<i64, Vec<SdeOperationItemReward>>,
    pub op_completion_rewards: HashMap<i64, Vec<SdeOperationItemReward>>,
    pub op_skill_rewards: HashMap<i64, Vec<u32>>,
    pub op_is_replayable: HashMap<i64, bool>,
    pub site_race_dungeons: HashMap<i64, [u32; 4]>,
    pub sde_tasks: HashSet<i64>,
    pub task_completion_rewards: HashMap<i64, Vec<(u32, u32)>>,
    pub task_event_names: HashMap<i64, String>,
    pub task_event_conditions: HashMap<i64, HashMap<String, Vec<String>>>,
    pub task_target_values: HashMap<i64, u32>,
    pub task_is_optional: HashSet<i64>,
    pub task_is_client_skippable: HashSet<i64>,
    pub category_isk_rewards: HashMap<i64, u64>,
    pub category_operations: HashMap<i64, Vec<i64>>,
    pub client_completable_tasks: HashSet<i64>,
}

impl OperationDefinitions {
    pub fn resolve_site_dungeon_id(
        &self,
        sde: &eve_sde::SdeRegistry,
        site_id: i64,
        race_id: u32,
    ) -> Option<u32> {
        if let Some(&[amarr, caldari, gallente, minmatar]) = self.site_race_dungeons.get(&site_id) {
            let preferred = match race_id {
                1 => caldari,
                2 => minmatar,
                4 => amarr,
                8 => gallente,
                _ => 0,
            };
            let candidate = if preferred > 0 {
                preferred
            } else {
                [caldari, minmatar, amarr, gallente]
                    .into_iter()
                    .find(|&d| d > 0)
                    .unwrap_or(0)
            };
            if candidate > 0 && sde.dungeons.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        u32::try_from(site_id)
            .ok()
            .filter(|id| sde.dungeons.contains_key(id))
    }

    pub fn are_prerequisites_met(&self, op_id: i64, completed_ops: &HashSet<i64>) -> bool {
        if let Some(prereqs) = self.op_prerequisites.get(&op_id) {
            if !prereqs.iter().all(|p| completed_ops.contains(p)) {
                return false;
            }
        }
        if let Some(parents) = self.op_tree_parents.get(&op_id) {
            if !parents.is_empty() && !parents.iter().any(|p| completed_ops.contains(p)) {
                return false;
            }
        }
        true
    }
}

fn parse_op_item_rewards(val: &serde_json::Value, key: &str) -> Vec<SdeOperationItemReward> {
    val.get(key)
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let tid = item.get("typeID")?.as_u64()? as u32;
                    let amt = item.get("amount").and_then(|x| x.as_u64()).unwrap_or(1).max(1) as u32;
                    let race_id = item.get("raceID").and_then(|x| x.as_u64()).map(|r| r as u32);
                    if tid == 0 {
                        None
                    } else {
                        Some(SdeOperationItemReward {
                            type_id: tid,
                            amount: amt,
                            race_id,
                        })
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn load_operation_definitions(sde: &eve_sde::SdeRegistry) -> OperationDefinitions {
    let mut defs = OperationDefinitions::default();
    if let Ok(ops) = sde.operations() {
        for (op_id_str, val) in ops {
            let Ok(op_id) = op_id_str.parse::<i64>() else { continue; };
            defs.sde_operations.insert(op_id);
            if let Some(tasks_arr) = val.get("tasksInOperation").and_then(|v| v.as_array()) {
                let mut task_ids = Vec::new();
                for t in tasks_arr {
                    if let Some(tid) = t.as_i64() {
                        task_ids.push(tid);
                        defs.task_to_op.insert(tid, op_id);
                    }
                }
                defs.op_tasks.insert(op_id, task_ids);
            }
            if let Some(prereq) = val.get("operationPrerequisites").and_then(|v| v.as_i64()).filter(|&p| p > 0) {
                defs.op_prerequisites.entry(op_id).or_default().push(prereq);
            }
            if let Some(site_id) = val.get("siteConnection").and_then(|v| v.as_i64()).filter(|&s| s > 0) {
                defs.op_site_ids.insert(op_id, site_id);
            }
            if let Some(isk) = val
                .get("iskReward")
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| f.is_finite() && *f >= 1.0).map(|f| f.round() as u64)))
                .filter(|&v| v > 0)
            {
                defs.op_isk_rewards.insert(op_id, isk);
            }
            let act_rws = parse_op_item_rewards(&val, "onActivationRewards");
            if !act_rws.is_empty() {
                defs.op_activation_rewards.insert(op_id, act_rws);
            }
            let comp_rws = parse_op_item_rewards(&val, "onCompletionRewards");
            if !comp_rws.is_empty() {
                defs.op_completion_rewards.insert(op_id, comp_rws);
            }
            if let Some(skills) = val.get("skillRewards").and_then(|v| v.as_array()) {
                let sk_ids: Vec<u32> = skills
                    .iter()
                    .filter_map(|x| x.as_u64().map(|n| n as u32))
                    .filter(|&id| id > 0)
                    .collect();
                if !sk_ids.is_empty() {
                    defs.op_skill_rewards.insert(op_id, sk_ids);
                }
            }
            let replayable = val
                .get("isReplayable")
                .and_then(|v| v.as_u64().map(|n| n != 0).or_else(|| v.as_bool()))
                .unwrap_or(false);
            defs.op_is_replayable.insert(op_id, replayable);
        }
    }
    let mut trees_by_id: HashMap<u32, Vec<i64>> = HashMap::new();
    if let Ok(trees) = sde.load_cfsd_nested("res:/staticdata/operationtreestructures.fsdbinary") {
        for (tid_str, val) in trees {
            let Ok(tree_id) = tid_str.parse::<u32>() else { continue; };
            let mut ops_in_tree = Vec::new();
            if let Some(conns) = val.get("connections").and_then(|v| v.as_object()) {
                for (parent_str, children_val) in conns {
                    let parent_op = parent_str
                        .parse::<i64>()
                        .ok()
                        .filter(|p| defs.sde_operations.contains(p));
                    if let Some(p) = parent_op {
                        ops_in_tree.push(p);
                    }
                    if let Some(arr) = children_val.as_array() {
                        for child_val in arr {
                            if let Some(c) = child_val.as_i64().filter(|c| defs.sde_operations.contains(c)) {
                                ops_in_tree.push(c);
                                if let Some(p) = parent_op.filter(|&p| p != c) {
                                    let entry = defs.op_tree_parents.entry(c).or_default();
                                    if !entry.contains(&p) {
                                        entry.push(p);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ops_in_tree.sort_unstable();
            ops_in_tree.dedup();
            trees_by_id.insert(tree_id, ops_in_tree);
        }
    }
    if let Ok(cats) = sde.load_cfsd_nested("res:/staticdata/operationcategories.fsdbinary") {
        for (cat_id_str, val) in cats {
            let Ok(cat_id) = cat_id_str.parse::<i64>() else { continue; };
            defs.sde_categories.insert(cat_id);
            if let Some(isk) = val
                .get("iskReward")
                .and_then(|v| v.as_u64().or_else(|| v.as_f64().filter(|f| f.is_finite() && *f >= 1.0).map(|f| f.round() as u64)))
                .filter(|&v| v > 0)
            {
                defs.cat_isk_rewards.insert(cat_id, isk);
            }
            if let Some(trees) = val.get("operationTrees").and_then(|v| v.as_array()) {
                let mut cat_ops = Vec::new();
                for x in trees {
                    if let Some(tree_id) = x.as_u64().map(|n| n as u32) {
                        if let Some(resolved) = trees_by_id.get(&tree_id) {
                            cat_ops.extend_from_slice(resolved);
                        } else if defs.sde_operations.contains(&i64::from(tree_id)) {
                            cat_ops.push(i64::from(tree_id));
                        }
                    }
                }
                cat_ops.sort_unstable();
                cat_ops.dedup();
                if !cat_ops.is_empty() {
                    for &oid in &cat_ops {
                        defs.op_default_category.entry(oid).or_insert(cat_id);
                    }
                    defs.cat_operation_trees.insert(cat_id, cat_ops);
                }
            }
        }
    }
    if let Ok(sites) = sde.load_cfsd_nested("res:/staticdata/operationsites.fsdbinary") {
        for (sid_str, val) in sites {
            let Ok(sid) = sid_str.parse::<i64>() else { continue; };
            if let Some(dids) = val.get("dungeonIDs") {
                let amarr = dids.get("amarr").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let caldari = dids.get("caldari").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let gallente = dids.get("gallente").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let minmatar = dids.get("minmatar").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                defs.site_race_dungeons
                    .insert(sid, [amarr, caldari, gallente, minmatar]);
            }
        }
    }
    let mut ach_events: HashMap<i64, String> = HashMap::new();
    if let Ok(ev_tbl) = sde.load_cfsd_table("res:/staticdata/achievementevents.fsdbinary") {
        for (ev_id, rec) in ev_tbl {
            if let Some(eve_sde::cfsd_flat_tables::CfsdValue::Str(name)) = rec.fields.get("name") {
                if !name.is_empty() {
                    ach_events.insert(ev_id, name.clone());
                }
            }
        }
    }
    if let Ok(tasks) = sde.operation_tasks() {
        for (tid_str, val) in tasks {
            let Ok(tid) = tid_str.parse::<i64>() else { continue; };
            defs.sde_tasks.insert(tid);
            if let Some(rws) = val.get("onCompletionRewards").and_then(|v| v.as_array()) {
                let parsed: Vec<(u32, u32)> = rws
                    .iter()
                    .filter_map(|item| {
                        let type_id = item.get("typeID")?.as_u64()? as u32;
                        let amount = item.get("amount").and_then(|x| x.as_u64()).unwrap_or(1).max(1) as u32;
                        (type_id > 0).then_some((type_id, amount))
                    })
                    .collect();
                if !parsed.is_empty() {
                    defs.task_completion_rewards.insert(tid, parsed);
                }
            }
            if let Some(ev_type) = val.get("achievementEventType").and_then(|v| v.as_i64()) {
                if let Some(ev_name) = ach_events.get(&ev_type) {
                    defs.task_event_names.insert(tid, ev_name.clone());
                }
            }
            if let Some(ev_obj) = val.get("eventConditions").and_then(|v| v.as_object()) {
                let mut cond_map: HashMap<String, Vec<String>> = HashMap::new();
                for (ck, cv) in ev_obj {
                    let allowed: Vec<String> = cv
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| {
                                    x.as_str()
                                        .map(str::to_string)
                                        .or_else(|| x.as_i64().map(|n| n.to_string()))
                                })
                                .filter(|s| !s.is_empty())
                                .collect()
                        })
                        .unwrap_or_default();
                    if !allowed.is_empty() {
                        cond_map.insert(ck.clone(), allowed);
                    }
                }
                if !cond_map.is_empty() {
                    defs.task_event_conditions.insert(tid, cond_map);
                }
            }
            let target = val
                .get("targetValue")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1) as u32;
            defs.task_target_values.insert(tid, target);
            if val
                .get("isOptional")
                .and_then(|v| v.as_bool().or_else(|| v.as_u64().map(|n| n != 0)))
                .unwrap_or(false)
            {
                defs.task_is_optional.insert(tid);
            }
            if val
                .get("clientSkipConditions")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| !arr.is_empty())
            {
                defs.task_is_client_skippable.insert(tid);
            }
        }
    }
    defs.category_isk_rewards = defs.cat_isk_rewards.clone();
    defs.category_operations = defs.cat_operation_trees.clone();
    for (&tid, ev_name) in &defs.task_event_names {
        if ev_name.starts_with("client_") || ev_name.contains("Client") {
            defs.client_completable_tasks.insert(tid);
        }
    }
    defs
}

#[derive(Debug, Clone, Default)]
pub struct OperationRewardOutcome {
    pub new_balance_cents: Option<i64>,
    pub enqueued_redeem_tokens: bool,
    pub created_hangar_items: Vec<(i64, u32, i64, u32)>,
}

fn deliver_op_item(
    snap: &mut eve_db::WorldStateSnapshot,
    out: &mut OperationRewardOutcome,
    uid: u64,
    char_id: i64,
    docked_loc: Option<i64>,
    type_id: u32,
    amount: u32,
    reason: &str,
) {
    if type_id == 0 || amount == 0 {
        return;
    }
    if let Some(loc_id) = docked_loc {
        snap.next_entity_id += 1;
        let item_id = snap.next_entity_id;
        snap.items.insert(
            item_id,
            eve_db::InventoryItemRecord {
                item_id,
                type_id,
                owner_id: char_id,
                location_id: loc_id,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: amount as i32,
                singleton: amount == 1,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        out.created_hangar_items.push((item_id, type_id, loc_id, amount));
    } else {
        crate::services::account_services::enqueue_redeem_token_in_snapshot(
            snap,
            uid,
            type_id,
            amount as i32,
            reason.to_string(),
            None,
            None,
        );
        out.enqueued_redeem_tokens = true;
    }
}

fn credit_op_isk(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    isk_amount: u64,
    now_unix: u64,
    entry_type: &str,
    reason: String,
) -> Option<i64> {
    if isk_amount == 0 {
        return None;
    }
    let add_cents = (isk_amount as i64).saturating_mul(100);
    let new_bal = {
        let ch = snap.characters.get_mut(&char_id)?;
        ch.balance = eve_db::IskCents(ch.balance.0.saturating_add(add_cents));
        ch.version = ch.version.wrapping_add(1);
        ch.balance.0
    };
    snap.next_entity_id += 1;
    let jid = snap.next_entity_id;
    snap.wallet_journal.push(eve_db::WalletJournalEntry {
        entry_id: jid,
        character_id: char_id,
        entry_type: entry_type.to_string(),
        amount: eve_db::IskCents(add_cents),
        balance_after: eve_db::IskCents(new_bal),
        reason,
    });
    snap.wallet_journal_timestamps.insert(jid, now_unix);
    Some(new_bal)
}

pub fn grant_operation_activation_rewards(
    snap: &mut eve_db::WorldStateSnapshot,
    rec: &mut CharacterOperationsRecord,
    op_defs: &OperationDefinitions,
    uid: u64,
    char_id: i64,
    sess_docked: Option<i64>,
    op_id: i64,
) -> OperationRewardOutcome {
    let mut out = OperationRewardOutcome::default();
    if !rec.rewarded_activations.insert(op_id) {
        return out;
    }
    let (char_race, docked_loc) = snap
        .characters
        .get(&char_id)
        .map(|c| (c.race_id, sess_docked.or(c.station_id).or(c.structure_id)))
        .unwrap_or((0, sess_docked));
    if let Some(rws) = op_defs.op_activation_rewards.get(&op_id) {
        let reason = format!("Operation {op_id} activation reward");
        for rw in rws {
            if rw.race_id.is_none_or(|r| r == 0 || r == char_race) {
                deliver_op_item(snap, &mut out, uid, char_id, docked_loc, rw.type_id, rw.amount, &reason);
            }
        }
    }
    out
}

pub fn grant_task_and_operation_rewards(
    snap: &mut eve_db::WorldStateSnapshot,
    rec: &mut CharacterOperationsRecord,
    op_defs: &OperationDefinitions,
    uid: u64,
    char_id: i64,
    sess_docked: Option<i64>,
    now_unix: u64,
    op_id: i64,
    task_id: Option<i64>,
    op_just_completed: bool,
) -> OperationRewardOutcome {
    let mut out = OperationRewardOutcome::default();
    let (char_race, docked_loc) = snap
        .characters
        .get(&char_id)
        .map(|c| (c.race_id, sess_docked.or(c.station_id).or(c.structure_id)))
        .unwrap_or((0, sess_docked));
    if let Some(tid) = task_id.filter(|t| rec.server_verified_tasks.contains(t)) {
        if rec.rewarded_tasks.insert(tid) {
            if let Some(rws) = op_defs.task_completion_rewards.get(&tid) {
                let reason = format!("Operation task {tid} completion reward");
                for &(type_id, amount) in rws {
                    deliver_op_item(snap, &mut out, uid, char_id, docked_loc, type_id, amount, &reason);
                }
            }
        }
    }
    let op_eligible_for_payout = op_just_completed
        && op_defs.op_tasks.get(&op_id).is_some_and(|ts| {
            ts.iter().any(|t| rec.server_verified_tasks.contains(t))
                && ts.iter().all(|t| {
                    rec.completed_tasks.contains(t)
                        || op_defs.task_is_optional.contains(t)
                        || op_defs.task_is_client_skippable.contains(t)
                })
        });
    if op_eligible_for_payout && rec.rewarded_operations.insert(op_id) {
        if let Some(&isk) = op_defs.op_isk_rewards.get(&op_id) {
            out.new_balance_cents = credit_op_isk(
                snap,
                char_id,
                isk,
                now_unix,
                "operation_reward",
                format!("AIR Operation {op_id} completion ISK reward"),
            );
        }
        let reason = format!("Operation {op_id} completion reward");
        if let Some(rws) = op_defs.op_completion_rewards.get(&op_id) {
            for rw in rws {
                if rw.race_id.is_none_or(|r| r == 0 || r == char_race) {
                    deliver_op_item(snap, &mut out, uid, char_id, docked_loc, rw.type_id, rw.amount, &reason);
                }
            }
        }
        if let Some(skills) = op_defs.op_skill_rewards.get(&op_id) {
            for &sk_tid in skills {
                deliver_op_item(snap, &mut out, uid, char_id, docked_loc, sk_tid, 1, &reason);
            }
        }
    }
    out
}

pub fn grant_category_completion_rewards(
    snap: &mut eve_db::WorldStateSnapshot,
    rec: &mut CharacterOperationsRecord,
    op_defs: &OperationDefinitions,
    char_id: i64,
    now_unix: u64,
    cat_id: i64,
) -> OperationRewardOutcome {
    let mut out = OperationRewardOutcome::default();
    if !rec.rewarded_categories.insert(cat_id) {
        return out;
    }
    if let Some(&isk) = op_defs.cat_isk_rewards.get(&cat_id) {
        out.new_balance_cents = credit_op_isk(
            snap,
            char_id,
            isk,
            now_unix,
            "operation_category_reward",
            format!("AIR Operation Category {cat_id} completion ISK reward"),
        );
    }
    out
}

pub fn emit_operation_reward_notifications_with_sink<F>(
    sde: &eve_sde::SdeRegistry,
    sink: F,
    char_id: i64,
    outcome: &OperationRewardOutcome,
) where
    F: Fn(NotificationTarget, &str, &str, Vec<PyRep>),
{
    if let Some(bal_cents) = outcome.new_balance_cents {
        sink(
            NotificationTarget::Character(char_id),
            "OnAccountChange",
            "charid",
            vec![
                PyRep::String("cash".into()),
                PyRep::Integer(char_id),
                PyRep::Real(eve_db::IskCents(bal_cents).to_f64_isk()),
            ],
        );
    }
    for &(item_id, type_id, loc_id, qty) in &outcome.created_hangar_items {
        let (group_id, category_id) = sde
            .types
            .get(&type_id)
            .map(|t| {
                let cid = sde
                    .groups
                    .get(&t.group_id)
                    .map(|g| g.category_id)
                    .unwrap_or(0);
                (t.group_id, cid)
            })
            .unwrap_or((0, 0));
        sink(
            NotificationTarget::Character(char_id),
            "OnItemChange",
            "charid",
            vec![
                PyRep::keyval([
                    ("itemID", PyRep::Integer(item_id)),
                    ("typeID", PyRep::Integer(i64::from(type_id))),
                    ("ownerID", PyRep::Integer(char_id)),
                    ("locationID", PyRep::Integer(loc_id)),
                    ("flagID", PyRep::Integer(eve_db::InventoryFlag::Hangar as i64)),
                    ("quantity", PyRep::Integer(i64::from(qty))),
                    ("stacksize", PyRep::Integer(i64::from(qty))),
                    ("groupID", PyRep::Integer(i64::from(group_id))),
                    ("categoryID", PyRep::Integer(i64::from(category_id))),
                    ("singleton", PyRep::Integer(i64::from(qty == 1))),
                ]),
                PyRep::Dict(vec![(PyRep::Integer(3), PyRep::Integer(0))]),
            ],
        );
    }
    if outcome.enqueued_redeem_tokens {
        sink(
            NotificationTarget::Character(char_id),
            "OnRedeemingTokensUpdated",
            "charid",
            vec![],
        );
    }
}

pub fn emit_operation_reward_notifications(
    ctx: &ServerContext,
    char_id: i64,
    outcome: &OperationRewardOutcome,
) {
    emit_operation_reward_notifications_with_sink(
        &ctx.sde,
        |target, event, id_type, args| {
            ctx.notify(target, event, id_type, args);
        },
        char_id,
        outcome,
    );
}

fn event_matches_task(event_name: &str, expected_ev: &str) -> bool {
    event_name.eq_ignore_ascii_case(expected_ev)
        || expected_ev
            .strip_prefix("client_")
            .is_some_and(|stripped| stripped.eq_ignore_ascii_case(event_name))
}

fn pyrep_matches_allowed(val: &PyRep, allowed: &[String]) -> bool {
    if let Some(s) = val.as_str() {
        allowed.iter().any(|a| a.eq_ignore_ascii_case(s))
    } else if let Some(n) = val.as_i64() {
        let ns = n.to_string();
        allowed.iter().any(|a| a == &ns)
    } else if let PyRep::List(items) | PyRep::Tuple(items) = val {
        items.iter().any(|item| pyrep_matches_allowed(item, allowed))
    } else {
        false
    }
}

fn extract_condition_pyrep<'a>(
    extra_data: Option<&'a PyRep>,
    cond_key: &str,
    cond_id: Option<i64>,
) -> Option<&'a PyRep> {
    match extra_data? {
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| {
                k.as_str().is_some_and(|ks| ks.eq_ignore_ascii_case(cond_key))
                    || (cond_id.is_some() && k.as_i64() == cond_id)
            })
            .map(|(_, v)| v),
        PyRep::KeyVal(_) => extra_data?.dict_get(cond_key),
        _ => None,
    }
}

fn check_client_extra_conditions(
    op_defs: &OperationDefinitions,
    sde: &eve_sde::SdeRegistry,
    tid: i64,
    extra_data: Option<&PyRep>,
    require_all_keys: bool,
) -> bool {
    let Some(conds) = op_defs.task_event_conditions.get(&tid) else {
        return true;
    };
    let cond_catalog = crate::services::season_fsd::fsd_challenge_conditions(sde);
    for (cond_key, allowed) in conds {
        if allowed.is_empty() {
            continue;
        }
        let cond_id = cond_catalog.get(cond_key).copied().map(|id| id as i64);
        match extract_condition_pyrep(extra_data, cond_key, cond_id) {
            Some(val) => {
                if !pyrep_matches_allowed(val, allowed) {
                    return false;
                }
            }
            None if require_all_keys => return false,
            None => {}
        }
    }
    true
}

fn advance_active_task_in_record(
    snap: &mut eve_db::WorldStateSnapshot,
    rec: &mut CharacterOperationsRecord,
    op_defs: &OperationDefinitions,
    uid: u64,
    char_id: i64,
    sess_docked: Option<i64>,
    now_unix: u64,
    op_id: i64,
    tid: i64,
    new_progress: u32,
    server_verified: bool,
) -> (Option<(i64, i64, i64)>, OperationRewardOutcome) {
    let target = op_defs.task_target_values.get(&tid).copied().unwrap_or(1).max(1);
    let clamped = new_progress.min(target);
    rec.task_progress.insert(tid, clamped);
    if server_verified {
        rec.server_verified_tasks.insert(tid);
    }
    if clamped < target {
        return (None, OperationRewardOutcome::default());
    }
    rec.completed_tasks.insert(tid);
    let cat_id = rec
        .active_category_id
        .or_else(|| op_defs.op_default_category.get(&op_id).copied())
        .unwrap_or(31);
    let all_sde_tasks_done = op_defs.op_tasks.get(&op_id).is_some_and(|ts| {
        !ts.is_empty()
            && ts.iter().all(|t| {
                rec.completed_tasks.contains(t)
                    || rec.skipped_tasks.contains(t)
                    || op_defs.task_is_optional.contains(t)
            })
    });
    if all_sde_tasks_done {
        rec.completed_operations.insert(op_id);
    }
    let out = grant_task_and_operation_rewards(
        snap,
        rec,
        op_defs,
        uid,
        char_id,
        sess_docked,
        now_unix,
        op_id,
        Some(tid),
        all_sde_tasks_done,
    );
    (Some((cat_id, op_id, tid)), out)
}

pub fn handle_operation_task_client_event(
    ctx: &ServerContext,
    session: &SessionState,
    uid: u64,
    char_id: i64,
    event_name: String,
    extra_data: Option<&PyRep>,
    has_call_args: bool,
) -> Result<PyRep, BridgeRpcError> {
    let op_defs = get_operation_defs(&ctx.sde);
    let now = ctx.now_unix();
    let sess_docked = session.station_id.or(session.structure_id);
    let sess_sys = session.solar_system_id;
    let is_gm = session.has_gm_role();
    let (completed_task, progressed, outcome) = ctx.db.with_mut_snapshot(|snap| {
        let curr_sys = sess_sys
            .or_else(|| snap.characters.get(&char_id).map(|c| c.solar_system_id))
            .unwrap_or(0);
        let mut store = load_store_pub(snap);
        let res = {
            let rec = store.by_character.entry(char_id).or_default();
            let active_op = rec
                .active_operation_id
                .filter(|oid| op_defs.sde_operations.contains(oid))
                .filter(|_| rec.active_operation_system_id.is_none_or(|s| s == curr_sys));
            let current_tid = active_op
                .and_then(|oid| op_defs.op_tasks.get(&oid))
                .and_then(|ts| {
                    ts.iter()
                        .copied()
                        .find(|t| !rec.completed_tasks.contains(t) && !rec.skipped_tasks.contains(t))
                });
            push_operation_event(rec, event_name.clone(), current_tid.unwrap_or(0));
            let mut transitioned = None;
            let mut did_progress = false;
            let mut out = OperationRewardOutcome::default();
            if let (Some(op_id), Some(tid)) = (active_op, current_tid) {
                if !rec.blocked_tasks.contains(&tid) {
                    if is_gm && matches!(event_name.as_str(), "complete_task" | "task_complete") {
                        let target_val = op_defs.task_target_values.get(&tid).copied().unwrap_or(1);
                        let (tr, rw) = advance_active_task_in_record(
                            snap, rec, op_defs, uid, char_id, sess_docked, now, op_id, tid, target_val, false,
                        );
                        transitioned = tr;
                        out = rw;
                        did_progress = true;
                    } else if let Some(expected_ev) = op_defs.task_event_names.get(&tid) {
                        if expected_ev.starts_with("client_")
                            && (event_matches_task(&event_name, expected_ev)
                                || matches!(event_name.as_str(), "complete_task" | "task_complete"))
                            && check_client_extra_conditions(op_defs, &ctx.sde, tid, extra_data, true)
                        {
                            let prev = rec.task_progress.get(&tid).copied().unwrap_or(0);
                            let (tr, rw) = advance_active_task_in_record(
                                snap, rec, op_defs, uid, char_id, sess_docked, now, op_id, tid, prev.saturating_add(1), false,
                            );
                            transitioned = tr;
                            out = rw;
                            did_progress = true;
                        }
                    }
                }
            }
            (transitioned, did_progress, out)
        };
        save_store_pub(snap, &store, now);
        res
    });
    emit_operation_reward_notifications(ctx, char_id, &outcome);
    if let Some((cat_id, op_id, tid)) = completed_task {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnOperationTaskCompleted",
            "charid",
            vec![
                PyRep::Integer(cat_id),
                PyRep::Integer(op_id),
                PyRep::Integer(tid),
            ],
        );
        Ok(PyRep::Bool(true))
    } else if progressed {
        Ok(PyRep::Bool(true))
    } else if has_call_args {
        Ok(PyRep::Bool(false))
    } else {
        Ok(PyRep::None)
    }
}

pub fn record_server_operation_event_in_db(
    db: &eve_db::WorldRepository,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
    event_name: &str,
    params: &[(&str, String)],
    increment: u32,
    now: u64,
) -> (Option<(i64, i64, i64)>, OperationRewardOutcome) {
    if char_id <= 0 || increment == 0 {
        return (None, OperationRewardOutcome::default());
    }
    let op_defs = get_operation_defs(sde);
    db.with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return (None, OperationRewardOutcome::default());
        };
        let uid = ch.user_id;
        let curr_sys = ch.solar_system_id;
        let docked_loc = ch.station_id.or(ch.structure_id);
        let mut store = load_store_pub(snap);
        let res = {
            let rec = store.by_character.entry(char_id).or_default();
            let active_op = rec
                .active_operation_id
                .filter(|oid| op_defs.sde_operations.contains(oid))
                .filter(|_| rec.active_operation_system_id.is_none_or(|s| s == curr_sys));
            let current_tid = active_op
                .and_then(|oid| op_defs.op_tasks.get(&oid))
                .and_then(|ts| {
                    ts.iter()
                        .copied()
                        .find(|t| !rec.completed_tasks.contains(t) && !rec.skipped_tasks.contains(t))
                });
            let mut transitioned = None;
            let mut out = OperationRewardOutcome::default();
            if let (Some(op_id), Some(tid)) = (active_op, current_tid) {
                if !rec.blocked_tasks.contains(&tid)
                    && op_defs
                        .task_event_names
                        .get(&tid)
                        .is_some_and(|ev| ev.eq_ignore_ascii_case(event_name))
                {
                    let conds_ok = op_defs.task_event_conditions.get(&tid).is_none_or(|conds| {
                        conds.iter().all(|(ck, allowed)| {
                            allowed.is_empty()
                                || params
                                    .iter()
                                    .find(|(pk, _)| pk.eq_ignore_ascii_case(ck))
                                    .is_some_and(|(_, pv)| allowed.iter().any(|a| a.eq_ignore_ascii_case(pv)))
                        })
                    });
                    if conds_ok {
                        let prev = rec.task_progress.get(&tid).copied().unwrap_or(0);
                        let (tr, rw) = advance_active_task_in_record(
                            snap, rec, op_defs, uid, char_id, docked_loc, now, op_id, tid, prev.saturating_add(increment), true,
                        );
                        transitioned = tr;
                        out = rw;
                    }
                }
            }
            (transitioned, out)
        };
        save_store_pub(snap, &store, now);
        res
    })
}

/// Authoritative server-side operation event hook invoked by server gameplay subsystems
/// (such as NPC destruction in `combat/npc_ai.rs` and dungeon entry in `start_site`).
pub fn record_server_operation_event(
    ctx: &ServerContext,
    char_id: i64,
    event_name: &str,
    params: &[(&str, String)],
    increment: u32,
) {
    let (completed_task, outcome) = record_server_operation_event_in_db(
        &ctx.db,
        &ctx.sde,
        char_id,
        event_name,
        params,
        increment,
        ctx.now_unix(),
    );
    emit_operation_reward_notifications(ctx, char_id, &outcome);
    if let Some((cat_id, op_id, tid)) = completed_task {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnOperationTaskCompleted",
            "charid",
            vec![
                PyRep::Integer(cat_id),
                PyRep::Integer(op_id),
                PyRep::Integer(tid),
            ],
        );
    }
}

/// Authoritative `invbroker.Add` gameplay hook for fitting modules, loading charges, and looting wrecks/containers in space.
pub fn record_invbroker_add_operation_event(
    ctx: &ServerContext,
    char_id: i64,
    orig_source_loc_id: i64,
    new_loc: i64,
    flag: eve_db::InventoryFlag,
    type_id: u32,
    qty: i32,
) {
    let (gid, cid) = ctx
        .sde
        .types
        .get(&type_id)
        .map(|t| (t.group_id, t.category_id))
        .unwrap_or((0, 0));
    let params = [
        ("typeID", type_id.to_string()),
        ("groupID", gid.to_string()),
        ("categoryID", cid.to_string()),
    ];
    let flag_u16 = flag as u16;
    if matches!(flag_u16, 11..=34 | 125..=132) {
        let dest_is_ship = ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&new_loc)
                .and_then(|s| ctx.sde.types.get(&s.type_id))
                .is_some_and(|t| t.category_id == 6)
        });
        if dest_is_ship {
            if cid == 8 {
                record_server_operation_event(
                    ctx,
                    char_id,
                    "loadChargesCount",
                    &params,
                    qty.max(1) as u32,
                );
            } else {
                record_server_operation_event(ctx, char_id, "fitItemCount", &params, 1);
            }
        }
        return;
    }
    if flag == eve_db::InventoryFlag::Cargo && orig_source_loc_id != new_loc && orig_source_loc_id > 0 {
        let from_space_wreck_or_container = ctx.db.with_snapshot(|snap| {
            let dest_is_ship = snap
                .items
                .get(&new_loc)
                .and_then(|s| ctx.sde.types.get(&s.type_id))
                .is_some_and(|t| t.category_id == 6);
            if !dest_is_ship {
                return false;
            }
            snap.items.get(&orig_source_loc_id).is_some_and(|src| {
                let in_space = (30_000_000..=32_999_999).contains(&src.location_id);
                let src_t = ctx.sde.types.get(&src.type_id);
                in_space
                    && src_t.is_some_and(|t| {
                        t.category_id == 22 || matches!(t.group_id, 12 | 186 | 340 | 448 | 649)
                    })
            })
        });
        if from_space_wreck_or_container {
            record_server_operation_event(
                ctx,
                char_id,
                "lootItemCount",
                &params,
                qty.max(1) as u32,
            );
        }
    }
}

/// Advances active SDE operation tasks from achievement gameplay events and emits MachoNet notifications.
pub fn advance_and_notify_server_operation_tasks(
    db: &eve_db::WorldRepository,
    sde: &eve_sde::SdeRegistry,
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    char_id: i64,
    event_names: &[&str],
    params: &[(&str, String)],
    delta: u64,
    now_unix: u64,
) {
    let inc = u32::try_from(delta).unwrap_or(u32::MAX);
    for &ev in event_names {
        let (completed_task, outcome) =
            record_server_operation_event_in_db(db, sde, char_id, ev, params, inc, now_unix);
        if completed_task.is_none()
            && outcome.new_balance_cents.is_none()
            && outcome.created_hangar_items.is_empty()
            && !outcome.enqueued_redeem_tokens
        {
            continue;
        }
        let Some(sink) = gateway
            .macho_notify_sink
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .cloned()
        else {
            continue;
        };
        emit_operation_reward_notifications_with_sink(sde, &*sink, char_id, &outcome);
        if let Some((cat_id, op_id, tid)) = completed_task {
            sink(
                NotificationTarget::Character(char_id),
                "OnOperationTaskCompleted",
                "charid",
                vec![
                    PyRep::Integer(cat_id),
                    PyRep::Integer(op_id),
                    PyRep::Integer(tid),
                ],
            );
        }
    }
}

static OPERATION_DEFS_CACHE: std::sync::OnceLock<OperationDefinitions> = std::sync::OnceLock::new();

pub fn get_operation_defs(sde: &eve_sde::SdeRegistry) -> &'static OperationDefinitions {
    OPERATION_DEFS_CACHE.get_or_init(|| load_operation_definitions(sde))
}
