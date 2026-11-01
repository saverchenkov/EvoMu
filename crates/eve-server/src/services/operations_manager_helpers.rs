//! Helper structures and catalog loaders for `operations_manager`.
//!
//! Provides SDE `res:/staticdata/surveys.fsdbinary` catalog parsing, operation definitions caching,
//! and survey eligibility checks.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use super::operations_manager::CharacterOperationsRecord;

/// Survey definition decoded from SDE `res:/staticdata/surveys.fsdbinary` or operator-scheduled surveys.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SurveyDefinition {
    pub survey_id: i64,
    pub name: String,
    pub description: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    #[serde(default)]
    pub start_unix: Option<u64>,
    #[serde(default)]
    pub expires_at_unix: Option<u64>,
    pub grace_period_days: u32,
    pub min_account_age_hours: u32,
    #[serde(default)]
    pub item_rewards: Vec<(u32, u32)>,
    #[serde(default)]
    pub vgs_rewards: Vec<u32>,
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

pub fn parse_dmy_to_unix(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let day: u64 = parts[0].trim().parse().ok()?;
    let month: u64 = parts[1].trim().parse().ok()?;
    let year: u64 = parts[2].trim().parse().ok()?;
    if !(1..=31).contains(&day) || !(1..=12).contains(&month) || year < 1970 {
        return None;
    }
    let mut days: u64 = 0;
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }
    let month_days = [
        31,
        if is_leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    for m in 1..month {
        days += month_days[(m - 1) as usize];
    }
    days += day.saturating_sub(1);
    Some(days * 86_400)
}

/// Loads survey definitions exclusively from SDE `res:/staticdata/surveys.fsdbinary`.
pub fn load_survey_catalog(sde: &eve_sde::SdeRegistry) -> Vec<SurveyDefinition> {
    let mut catalog = Vec::new();
    if let Ok(surveys_map) = sde.surveys() {
        for (id_str, val) in surveys_map {
            let Ok(sid) = id_str.parse::<i64>() else { continue; };
            let name = val.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let desc = val.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let start = val.get("startDate").and_then(|v| v.as_str()).map(str::to_string);
            let end = val.get("endDate").and_then(|v| v.as_str()).map(str::to_string);
            let grace = val.get("gracePeriod").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let min_age = val.get("minAccountAgeHours").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let start_unix = start.as_deref().and_then(parse_dmy_to_unix);
            let expires_at_unix = end.as_deref().and_then(parse_dmy_to_unix).map(|exp| {
                exp.saturating_add((u64::from(grace) + 1) * 86_400 - 1)
            });

            let item_rewards = val
                .get("itemRewards")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let tid = item
                                .get("type_id")
                                .or_else(|| item.get("typeID"))
                                .and_then(|x| x.as_u64())? as u32;
                            let amt = item
                                .get("amount")
                                .or_else(|| item.get("quantity"))
                                .and_then(|x| x.as_u64())
                                .unwrap_or(1)
                                .max(1) as u32;
                            Some((tid, amt))
                        })
                        .collect()
                })
                .unwrap_or_default();

            let vgs_rewards = val
                .get("vgsRewards")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_u64().map(|n| n as u32))
                        .collect()
                })
                .unwrap_or_default();

            catalog.push(SurveyDefinition {
                survey_id: sid,
                name,
                description: desc,
                start_date: start,
                end_date: end,
                start_unix,
                expires_at_unix,
                grace_period_days: grace,
                min_account_age_hours: min_age,
                item_rewards,
                vgs_rewards,
            });
        }
    }
    catalog.sort_by_key(|s| s.survey_id);
    catalog
}

/// Merges the static SDE survey catalog with any operator-registered survey instances.
pub fn combined_survey_catalog(
    sde: &eve_sde::SdeRegistry,
    custom_surveys: &[SurveyDefinition],
) -> Vec<SurveyDefinition> {
    let mut catalog = get_survey_catalog(sde).to_vec();
    for custom in custom_surveys {
        if let Some(existing) = catalog.iter_mut().find(|s| s.survey_id == custom.survey_id) {
            *existing = custom.clone();
        } else {
            catalog.push(custom.clone());
        }
    }
    catalog.sort_by_key(|s| s.survey_id);
    catalog
}

/// Operation definitions, task mappings, and SDE rewards loaded from
/// `operationcategories.fsdbinary`, `operationtreestructures.fsdbinary`,
/// `operations.fsdbinary`, and `operationtasks.fsdbinary`.
#[derive(Debug, Clone, Default)]
pub struct OperationDefinitions {
    pub valid_categories: std::collections::HashSet<i64>,
    pub category_operations: HashMap<i64, Vec<i64>>,
    pub op_prerequisites: HashMap<i64, Vec<i64>>,
    pub op_tasks: HashMap<i64, Vec<i64>>,
    pub task_to_op: HashMap<i64, i64>,
    pub task_event_names: HashMap<i64, String>,
    pub client_completable_tasks: std::collections::HashSet<i64>,
    pub category_isk_rewards: HashMap<i64, i64>,
    pub op_isk_rewards: HashMap<i64, i64>,
    pub op_activation_rewards: HashMap<i64, Vec<(u32, u32, Option<u32>)>>,
    pub op_completion_rewards: HashMap<i64, Vec<(u32, u32, Option<u32>)>>,
    pub task_completion_rewards: HashMap<i64, Vec<(u32, u32)>>,
}

fn parse_op_rewards(arr: Option<&Vec<serde_json::Value>>) -> Vec<(u32, u32, Option<u32>)> {
    arr.map(|list| {
        list.iter()
            .filter_map(|item| {
                let tid = item.get("typeID").and_then(|v| v.as_u64())? as u32;
                if tid == 0 {
                    return None;
                }
                let amt = item.get("amount").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as u32;
                let race_id = item
                    .get("raceID")
                    .and_then(|v| v.as_u64())
                    .map(|r| r as u32)
                    .filter(|&r| r > 0);
                Some((tid, amt, race_id))
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
            if let Some(tasks_arr) = val.get("tasksInOperation").and_then(|v| v.as_array()) {
                let mut task_ids = Vec::new();
                for t in tasks_arr {
                    if let Some(tid) = t.as_i64() {
                        task_ids.push(tid);
                        defs.task_to_op.insert(tid, op_id);
                    }
                }
                defs.op_tasks.insert(op_id, task_ids);
            } else {
                defs.op_tasks.insert(op_id, Vec::new());
            }
            if let Some(isk) = val.get("iskReward").and_then(|v| v.as_i64()).filter(|&x| x > 0) {
                defs.op_isk_rewards.insert(op_id, isk);
            }
            let act_rws = parse_op_rewards(val.get("onActivationRewards").and_then(|v| v.as_array()));
            if !act_rws.is_empty() {
                defs.op_activation_rewards.insert(op_id, act_rws);
            }
            let cmp_rws = parse_op_rewards(val.get("onCompletionRewards").and_then(|v| v.as_array()));
            if !cmp_rws.is_empty() {
                defs.op_completion_rewards.insert(op_id, cmp_rws);
            }
        }
    }
    let mut tree_ops: HashMap<i64, Vec<i64>> = HashMap::new();
    if let Ok(trees) = sde.load_cfsd_nested("res:/staticdata/operationtreestructures.fsdbinary") {
        for (tree_id_str, val) in trees {
            let Ok(tree_id) = tree_id_str.parse::<i64>() else { continue };
            let mut ops_in_tree = Vec::new();
            if let Some(conns) = val.get("connections").and_then(|v| v.as_object()) {
                for (src_str, dst_val) in conns {
                    let src_opt = src_str.parse::<i64>().ok().filter(|oid| defs.op_tasks.contains_key(oid));
                    if let Some(src_op) = src_opt {
                        if !ops_in_tree.contains(&src_op) {
                            ops_in_tree.push(src_op);
                        }
                    }
                    if let Some(arr) = dst_val.as_array() {
                        for d in arr {
                            if let Some(dst_op) = d.as_i64().filter(|oid| defs.op_tasks.contains_key(oid)) {
                                if !ops_in_tree.contains(&dst_op) {
                                    ops_in_tree.push(dst_op);
                                }
                                if let Some(src_op) = src_opt {
                                    let prereqs = defs.op_prerequisites.entry(dst_op).or_default();
                                    if src_op != dst_op && !prereqs.contains(&src_op) {
                                        prereqs.push(src_op);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            ops_in_tree.sort_unstable();
            tree_ops.insert(tree_id, ops_in_tree);
        }
    }
    if let Ok(cats) = sde.operation_categories() {
        for (cat_id_str, val) in cats {
            let Ok(cat_id) = cat_id_str.parse::<i64>() else { continue };
            defs.valid_categories.insert(cat_id);
            if let Some(isk) = val.get("iskReward").and_then(|v| v.as_i64()).filter(|&x| x > 0) {
                defs.category_isk_rewards.insert(cat_id, isk);
            }
            let mut cat_ops = Vec::new();
            if let Some(trees_arr) = val.get("operationTrees").and_then(|v| v.as_array()) {
                for tv in trees_arr {
                    if let Some(tid) = tv.as_i64() {
                        if let Some(ops_list) = tree_ops.get(&tid) {
                            for &oid in ops_list {
                                if !cat_ops.contains(&oid) {
                                    cat_ops.push(oid);
                                }
                            }
                        }
                    }
                }
            }
            if !cat_ops.is_empty() {
                defs.category_operations.insert(cat_id, cat_ops);
            }
        }
    }
    let events = sde
        .load_cfsd_table("res:/staticdata/achievementevents.fsdbinary")
        .unwrap_or_default();
    let achs = sde.load_achievements().unwrap_or_default();
    let mut client_eids = std::collections::HashSet::new();
    let mut server_eids = std::collections::HashSet::new();
    for info in achs.values().filter(|i| i.is_enabled) {
        for cond in &info.conditions {
            if info.is_client_achievement {
                client_eids.insert(cond.statistic_event_id);
            } else {
                server_eids.insert(cond.statistic_event_id);
            }
        }
    }
    if let Ok(tasks) = sde.operation_tasks() {
        for (tid_str, val) in tasks {
            let Ok(tid) = tid_str.parse::<i64>() else { continue };
            let eid = val.get("achievementEventType").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let ev_name = events
                .get(&i64::from(eid))
                .and_then(|r| r.str("name"))
                .unwrap_or("")
                .trim();
            if !ev_name.is_empty() {
                defs.task_event_names.insert(tid, ev_name.to_string());
            }
            if eid == 0
                || client_eids.contains(&eid)
                || (ev_name.starts_with("client_") && !server_eids.contains(&eid))
            {
                defs.client_completable_tasks.insert(tid);
            }
            if let Some(arr) = val.get("onCompletionRewards").and_then(|v| v.as_array()) {
                let rws: Vec<(u32, u32)> = arr
                    .iter()
                    .filter_map(|item| {
                        let type_id = item.get("typeID").and_then(|v| v.as_u64())? as u32;
                        if type_id == 0 {
                            return None;
                        }
                        let amt = item.get("amount").and_then(|v| v.as_u64()).unwrap_or(1).max(1) as u32;
                        Some((type_id, amt))
                    })
                    .collect();
                if !rws.is_empty() {
                    defs.task_completion_rewards.insert(tid, rws);
                }
            }
        }
    }
    defs
}

/// Grants once-per-key SDE ISK and/or item rewards for an operation, task, or category transition.
pub fn grant_operation_reward_bundle(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    reward_key: &str,
    claimed: &mut std::collections::HashSet<String>,
    isk_reward: i64,
    items: &[(u32, u32, Option<u32>)],
    now_unix: u64,
) -> Option<eve_db::IskCents> {
    if isk_reward <= 0 && items.is_empty() {
        return None;
    }
    let (char_race, delivery_loc_opt) = snap
        .characters
        .get(&char_id)
        .map(|c| {
            let loc = c
                .station_id
                .or(c.structure_id)
                .or_else(|| (c.home_station_id > 0).then_some(c.home_station_id));
            (c.race_id, loc)
        })
        .unwrap_or((0, None));
    if !items.is_empty() && delivery_loc_opt.is_none() && isk_reward <= 0 {
        return None;
    }
    if !claimed.insert(reward_key.to_string()) {
        return None;
    }
    if let Some(delivery_loc) = delivery_loc_opt {
        for &(type_id, qty, race_filter) in items {
            if let Some(req_race) = race_filter {
                if char_race != 0 && req_race != char_race {
                    continue;
                }
            }
            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id,
                    owner_id: char_id,
                    location_id: delivery_loc,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: qty.max(1) as i32,
                    singleton: qty == 1,
                    ..Default::default()
                },
            );
        }
    }
    if isk_reward > 0 {
        let amt = eve_db::IskCents::from_isk(isk_reward);
        let bal_after = if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = ch.balance.checked_add(amt).unwrap_or(ch.balance);
            ch.balance
        } else {
            amt
        };
        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "operation_reward".to_string(),
            amount: amt,
            balance_after: bal_after,
            reason: format!("AIR Operation reward ({reward_key})"),
        });
        snap.wallet_journal_timestamps.insert(jid, now_unix);
        return Some(bal_after);
    }
    None
}

/// Applies task and (when all tasks in `op_id` are finished) operation completion rewards.
pub fn grant_task_and_op_completion_rewards(
    snap: &mut eve_db::WorldStateSnapshot,
    op_defs: &OperationDefinitions,
    char_id: i64,
    tid: i64,
    op_id: i64,
    op_all_done: bool,
    claimed: &mut std::collections::HashSet<String>,
    now_unix: u64,
) -> Option<eve_db::IskCents> {
    if let Some(trws) = op_defs.task_completion_rewards.get(&tid) {
        let mapped: Vec<(u32, u32, Option<u32>)> = trws.iter().map(|&(t, a)| (t, a, None)).collect();
        let _ = grant_operation_reward_bundle(snap, char_id, &format!("task:{tid}"), claimed, 0, &mapped, now_unix);
    }
    if op_all_done {
        let op_isk = op_defs.op_isk_rewards.get(&op_id).copied().unwrap_or(0);
        let op_rws = op_defs.op_completion_rewards.get(&op_id).map(Vec::as_slice).unwrap_or(&[]);
        return grant_operation_reward_bundle(snap, char_id, &format!("op:{op_id}"), claimed, op_isk, op_rws, now_unix);
    }
    None
}

static SURVEY_CATALOG_CACHE: std::sync::OnceLock<Vec<SurveyDefinition>> = std::sync::OnceLock::new();
static OPERATION_DEFS_CACHE: std::sync::OnceLock<OperationDefinitions> = std::sync::OnceLock::new();

pub fn get_survey_catalog(sde: &eve_sde::SdeRegistry) -> &'static [SurveyDefinition] {
    SURVEY_CATALOG_CACHE.get_or_init(|| load_survey_catalog(sde))
}

pub fn get_operation_defs(sde: &eve_sde::SdeRegistry) -> &'static OperationDefinitions {
    OPERATION_DEFS_CACHE.get_or_init(|| load_operation_definitions(sde))
}

/// Checks survey eligibility per V24 `surveys/survey.pyj`:
/// - survey must not already be claimed by this character
/// - `now_unix` must fall within `[start_unix, expires_at_unix]` (endDate + gracePeriod days)
/// - character/account age must be at least `min_account_age_hours`
pub fn eligible_surveys(
    rec: &CharacterOperationsRecord,
    char_create_unix: u64,
    catalog: &[SurveyDefinition],
    now_unix: u64,
) -> Vec<i64> {
    let mut out = Vec::new();
    for survey in catalog {
        let sid = survey.survey_id;
        if rec.completed_surveys.contains(&sid) || rec.claimed_surveys.contains(&sid) {
            continue;
        }
        if let Some(start) = survey.start_unix {
            if now_unix < start {
                continue;
            }
        }
        if let Some(exp) = survey.expires_at_unix {
            if now_unix > exp {
                continue;
            }
        }
        if survey.min_account_age_hours > 0 && char_create_unix > 0 {
            let age_secs = now_unix.saturating_sub(char_create_unix);
            if age_secs < u64::from(survey.min_account_age_hours) * 3_600 {
                continue;
            }
        }
        out.push(sid);
    }
    out
}

/// Builds a [`SurveyDefinition`] for `/survey schedule <survey_id> [duration_secs] [type_id:qty,...] [vgs_id,...]`,
/// inheriting metadata from SDE `surveys.fsdbinary` when `survey_id` matches an SDE entry.
pub fn build_scheduled_survey_from_slash(
    sde: &eve_sde::SdeRegistry,
    now_unix: u64,
    parts: &[&str],
) -> Result<SurveyDefinition, String> {
    let survey_id = parts
        .first()
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|&id| id > 0)
        .ok_or_else(|| "Usage: /survey schedule <survey_id> [duration_secs] [type_id:qty,...] [vgs_id,...]".to_string())?;
    let duration_secs = match parts.get(1) {
        Some(s) => s
            .parse::<u64>()
            .ok()
            .filter(|&d| d > 0)
            .ok_or_else(|| "duration_secs must be a positive integer".to_string())?,
        None => 86_400,
    };
    let sde_base = get_survey_catalog(sde)
        .iter()
        .find(|s| s.survey_id == survey_id)
        .cloned();
    let mut item_rewards = sde_base
        .as_ref()
        .map(|s| s.item_rewards.clone())
        .unwrap_or_default();
    if let Some(items_arg) = parts.get(2).copied().filter(|s| !s.is_empty() && *s != "-") {
        let mut parsed_items = Vec::new();
        for pair in items_arg.split(',') {
            let (tid_str, qty_str) = pair
                .split_once(':')
                .ok_or_else(|| format!("Invalid item reward '{pair}', expected type_id:qty"))?;
            let tid = tid_str
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|id| sde.types.contains_key(id))
                .ok_or_else(|| format!("Unknown or invalid reward type_id '{tid_str}'"))?;
            let qty = qty_str
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&q| q > 0)
                .ok_or_else(|| format!("Invalid reward quantity '{qty_str}'"))?;
            parsed_items.push((tid, qty));
        }
        item_rewards = parsed_items;
    }
    let mut vgs_rewards = sde_base
        .as_ref()
        .map(|s| s.vgs_rewards.clone())
        .unwrap_or_default();
    if let Some(vgs_arg) = parts.get(3).copied().filter(|s| !s.is_empty() && *s != "-") {
        let mut parsed_vgs = Vec::new();
        for tok in vgs_arg.split(',') {
            let vid = tok
                .trim()
                .parse::<u32>()
                .ok()
                .filter(|&v| v > 0)
                .ok_or_else(|| format!("Invalid VGS reward ID '{tok}'"))?;
            parsed_vgs.push(vid);
        }
        vgs_rewards = parsed_vgs;
    }
    Ok(SurveyDefinition {
        survey_id,
        name: sde_base
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("Capsuleer Survey {survey_id}")),
        description: sde_base
            .as_ref()
            .map(|s| s.description.clone())
            .unwrap_or_else(|| "Active player satisfaction survey".to_string()),
        start_date: sde_base.as_ref().and_then(|s| s.start_date.clone()),
        end_date: sde_base.as_ref().and_then(|s| s.end_date.clone()),
        start_unix: Some(now_unix.saturating_sub(3_600)),
        expires_at_unix: Some(now_unix.saturating_add(duration_secs)),
        grace_period_days: sde_base.as_ref().map(|s| s.grace_period_days).unwrap_or(7),
        min_account_age_hours: sde_base
            .as_ref()
            .map(|s| s.min_account_age_hours)
            .unwrap_or(24),
        item_rewards,
        vgs_rewards,
    })
}

/// Verifies whether `complete_category_for_character` is permitted: either GM/QA role is held,
/// or `cat_id` is an SDE operation category whose constituent operations are all completed.
pub fn can_complete_operation_category(
    op_defs: &OperationDefinitions,
    is_gm: bool,
    rec: &super::operations_manager::CharacterOperationsRecord,
    cat_id: i64,
) -> bool {
    if is_gm {
        return true;
    }
    op_defs
        .category_operations
        .get(&cat_id)
        .is_some_and(|ops| !ops.is_empty() && ops.iter().all(|oid| rec.completed_operations.contains(oid)))
}

/// Advances any active SDE operation task matching a server-observed gameplay `event_names` hook
/// (`undockFromStation`, `timesWarped`, `stargateJumps`, `dockInStation`, `destroyHostileNPC`, etc.).
pub fn advance_server_operation_tasks_in_db(
    db: &eve_db::WorldRepository,
    sde: &eve_sde::SdeRegistry,
    char_id: i64,
    event_names: &[&str],
    now_unix: u64,
) -> Vec<(i64, i64, i64, Option<eve_db::IskCents>)> {
    if char_id <= 0 || event_names.is_empty() {
        return Vec::new();
    }
    let op_defs = get_operation_defs(sde);
    db.with_mut_snapshot(|snap| {
        let mut store = super::operations_manager::load_store(snap);
        let curr_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id);
        let Some(active_op) = store
            .by_character
            .get(&char_id)
            .filter(|r| r.active_operation_system_id.is_none_or(|op_s| curr_sys == Some(op_s)))
            .and_then(|r| r.active_operation_id)
        else {
            return Vec::new();
        };
        let Some(tasks) = op_defs.op_tasks.get(&active_op) else {
            return Vec::new();
        };
        let mut completed_out = Vec::new();
        let matching_tids: Vec<i64> = {
            let rec = store.by_character.entry(char_id).or_default();
            tasks
                .iter()
                .copied()
                .filter(|tid| {
                    !rec.completed_tasks.contains(tid)
                        && !rec.skipped_tasks.contains(tid)
                        && !rec.blocked_tasks.contains(tid)
                        && op_defs
                            .task_event_names
                            .get(tid)
                            .is_some_and(|en| event_names.contains(&en.as_str()))
                })
                .collect()
        };
        for tid in matching_tids {
            let (cat_id, op_all_done, mut claimed) = {
                let rec = store.by_character.entry(char_id).or_default();
                rec.completed_tasks.insert(tid);
                let cat_id = rec.active_category_id.unwrap_or(1);
                let all_done = !tasks.is_empty()
                    && tasks
                        .iter()
                        .all(|t| rec.completed_tasks.contains(t) || rec.skipped_tasks.contains(t));
                if all_done {
                    rec.completed_operations.insert(active_op);
                }
                (cat_id, all_done, std::mem::take(&mut rec.claimed_operation_rewards))
            };
            let bal = grant_task_and_op_completion_rewards(
                snap,
                op_defs,
                char_id,
                tid,
                active_op,
                op_all_done,
                &mut claimed,
                now_unix,
            );
            store.by_character.entry(char_id).or_default().claimed_operation_rewards = claimed;
            completed_out.push((cat_id, active_op, tid, bal));
        }
        if !completed_out.is_empty() {
            super::operations_manager::save_store(snap, &store, now_unix);
        }
        completed_out
    })
}

/// Advances active SDE operation tasks for `char_id` and emits `OnAccountChange('cash')` + `OnOperationTaskCompleted`.
pub fn advance_and_notify_server_operation_tasks(
    db: &eve_db::WorldRepository,
    sde: &eve_sde::SdeRegistry,
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    char_id: i64,
    event_names: &[&str],
    now_unix: u64,
) {
    let completed = advance_server_operation_tasks_in_db(db, sde, char_id, event_names, now_unix);
    if completed.is_empty() {
        return;
    }
    let Some(sink) = gateway
        .macho_notify_sink
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    else {
        return;
    };
    for (cat_id, op_id, tid, bal) in completed {
        if let Some(b) = bal {
            sink(
                crate::router::NotificationTarget::Character(char_id),
                "OnAccountChange",
                "charid",
                vec![
                    eve_proto::PyRep::String("cash".into()),
                    eve_proto::PyRep::Integer(char_id),
                    eve_proto::PyRep::Real(b.to_wire_f64()),
                ],
            );
        }
        sink(
            crate::router::NotificationTarget::Character(char_id),
            "OnOperationTaskCompleted",
            "charid",
            vec![
                eve_proto::PyRep::Integer(cat_id),
                eve_proto::PyRep::Integer(op_id),
                eve_proto::PyRep::Integer(tid),
            ],
        );
    }
}

/// Resolves the character's active operation/tutorial dungeon tuple `(dungeon_id, room_id, instance_id, dungeon_name)`
/// if the character has started an operation site (`start_site`) and is still in that solar system.
pub fn active_operation_dungeon_for_character(
    ctx: & crate::router::ServerContext,
    char_id: i64,
) -> Option<(u32, u32, i64, String)> {
    let (dungeon_id, site_id) = ctx.db.with_snapshot(|snap| {
        let store = super::operations_manager::load_store(snap);
        let rec = store.by_character.get(&char_id)?;
        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id);
        if let Some(op_sys) = rec.active_operation_system_id {
            if char_sys != Some(op_sys) {
                return None;
            }
        }
        Some((rec.active_dungeon_id?, rec.active_site_id?))
    })?;
    let d = ctx.sde.dungeons.get(&dungeon_id)?;
    let dname = d.name.clone();
    // DERIVED: When an SDE dungeon has no asteroid room row in `sde_dungeon_asteroid_counts`,
    // derive a deterministic primary room ID from `dungeon_id * 100 + 1`.
    let room_id = ctx
        .sde
        .dungeon_asteroids
        .get(&dungeon_id)
        .and_then(|rows| rows.first().map(|r| r.room_id))
        .unwrap_or(dungeon_id.saturating_mul(100).saturating_add(1));
    Some((dungeon_id, room_id, site_id, dname))
}

/// Resolves the NPE/Career operation spawnpoint solar system for `(char_id, operation_id)`.
pub fn resolve_operation_spawnpoint_solar_system(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    operation_id: Option<i64>,
) -> Option<u32> {
    ctx.db.with_snapshot(|snap| {
        let store = super::operations_manager::load_store(snap);
        if let Some(rec) = store.by_character.get(&char_id) {
            if let Some(sys_id) = rec.active_operation_system_id {
                if operation_id.is_none() || rec.active_operation_id == operation_id {
                    return Some(sys_id);
                }
            }
        }
        if let Some(ch) = snap.characters.get(&char_id) {
            if let Some(school) = ctx.sde.schools.get(&ch.school_id) {
                if school.solar_system_id > 0 {
                    return Some(school.solar_system_id);
                }
            }
            if let Some(school) = ctx
                .sde
                .schools
                .values()
                .find(|s| s.race_id == ch.race_id && s.solar_system_id > 0)
            {
                return Some(school.solar_system_id);
            }
            if ch.solar_system_id > 0 && ctx.sde.solar_systems.contains_key(&ch.solar_system_id) {
                return Some(ch.solar_system_id);
            }
        }
        None
    })
}

/// Checks whether `target_id` is an active operation site or system for `char_id`.
pub fn is_operation_site_or_system(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    target_id: i64,
) -> bool {
    if target_id <= 0 {
        return false;
    }
    ctx.db.with_snapshot(|snap| {
        let store = super::operations_manager::load_store(snap);
        let Some(rec) = store.by_character.get(&char_id) else {
            return false;
        };
        let char_sys = snap.characters.get(&char_id).map(|c| c.solar_system_id);
        if let Some(op_sys) = rec.active_operation_system_id {
            if char_sys != Some(op_sys) {
                return false;
            }
        }
        rec.active_operation_id == Some(target_id)
            || rec.active_site_id == Some(target_id)
            || rec.active_dungeon_id.map(i64::from) == Some(target_id)
            || rec.active_operation_system_id.map(i64::from) == Some(target_id)
    })
}
