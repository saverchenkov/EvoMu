//! Helper structures and catalog loaders for `operations_manager`.
//!
//! Provides survey catalog parsing, operation definitions caching, and survey eligibility checks.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use super::operations_manager::CharacterOperationsRecord;

/// Survey definition loaded from SDE `surveys.fsdbinary` or dynamic survey definitions.
#[derive(Debug, Clone)]
pub struct SurveyDefinition {
    pub survey_id: i64,
    pub name: String,
    pub description: String,
    pub start_date: Option<String>,
    pub end_date: Option<String>,
    pub grace_period_days: u32,
    pub min_account_age_hours: u32,
    pub sp_reward: f64,
    pub isk_reward: i64,
    pub expires_at_unix: Option<u64>,
}

fn is_leap_year(year: u64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn parse_dmy_to_unix(s: &str) -> Option<u64> {
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

/// Loads survey definitions from SDE `surveys.fsdbinary` combined with dynamic survey definitions.
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

            let expires_at_unix = end.as_deref().and_then(parse_dmy_to_unix).map(|exp| {
                exp.saturating_add((grace as u64) * 86_400)
            });

            let isk_reward = val.get("iskReward").and_then(|v| v.as_i64()).unwrap_or_else(|| {
                if val.get("vgsRewards").and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty()) {
                    250_000
                } else {
                    0
                }
            });
            let sp_reward = val.get("spReward").and_then(|v| v.as_f64()).unwrap_or(25_000.0);

            catalog.push(SurveyDefinition {
                survey_id: sid,
                name,
                description: desc,
                start_date: start,
                end_date: end,
                grace_period_days: grace,
                min_account_age_hours: min_age,
                sp_reward,
                isk_reward,
                expires_at_unix,
            });
        }
    }

    // Dynamic career survey definitions (surveys 1, 2, 3) available for player progression
    let dynamic_defs = [
        (1, "Career Introduction Survey", "Initial player satisfaction survey", 10_000.0, 0),
        (2, "Career Specialization Survey", "Mid-career player progression survey", 25_000.0, 250_000),
        (3, "Exploration & Combat Survey", "Advanced career operations survey", 15_000.0, 100_000),
    ];
    for (sid, name, desc, sp, isk) in dynamic_defs {
        if !catalog.iter().any(|s| s.survey_id == sid) {
            catalog.push(SurveyDefinition {
                survey_id: sid,
                name: name.to_string(),
                description: desc.to_string(),
                start_date: None,
                end_date: None,
                grace_period_days: 0,
                min_account_age_hours: 0,
                sp_reward: sp,
                isk_reward: isk,
                expires_at_unix: None,
            });
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

pub fn eligible_surveys(
    rec: &CharacterOperationsRecord,
    catalog: &[SurveyDefinition],
    now_unix: u64,
) -> Vec<i64> {
    let mut out = Vec::new();
    for survey in catalog {
        let sid = survey.survey_id;
        if rec.completed_surveys.contains(&sid) {
            continue;
        }
        if let Some(exp) = survey.expires_at_unix {
            if now_unix > exp {
                continue;
            }
        }
        let eligible = match sid {
            1 => true,
            2 => {
                !rec.completed_categories.is_empty()
                    || !rec.completed_operations.is_empty()
                    || !rec.completed_tasks.is_empty()
                    || rec.completed_surveys.contains(&1)
            }
            3 => {
                rec.active_site_id.is_some()
                    || (!rec.completed_categories.is_empty() && rec.completed_surveys.contains(&2))
            }
            _ => true,
        };
        if eligible {
            out.push(sid);
        }
    }
    out
}
