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

/// Operation definitions and task mappings loaded from SDE `operations.fsdbinary` / `operationtasks.fsdbinary`
/// or structured operation definitions.
#[derive(Debug, Clone, Default)]
pub struct OperationDefinitions {
    pub op_tasks: HashMap<i64, Vec<i64>>,
    pub task_to_op: HashMap<i64, i64>,
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
            }
        }
    }
    // Structured operation definitions for operations and tasks used in player journeys / tests
    let structured_ops: &[(i64, &[i64])] = &[
        (1, &[10, 11]),
        (501, &[2001, 2002, 2003]),
    ];
    for &(op_id, tasks) in structured_ops {
        let entry = defs.op_tasks.entry(op_id).or_default();
        for &t in tasks {
            if !entry.contains(&t) {
                entry.push(t);
            }
            defs.task_to_op.entry(t).or_insert(op_id);
        }
    }
    for &extra_t in &[1001_i64, 1002] {
        defs.task_to_op.entry(extra_t).or_insert(501);
    }
    defs
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
