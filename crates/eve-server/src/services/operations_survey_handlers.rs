//! In-game player satisfaction CSAT survey catalog, slash scheduling, and RPC handlers.
//!
//! Extracted from `operations_manager` and `operations_manager_helpers` to keep each service
//! module compact and well below the strict production file size budget.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use crate::router::{NotificationTarget, ServerContext};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use super::operations_manager::{CharacterOperationsRecord, load_store_pub, save_store_pub};

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

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
        31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
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

static SURVEY_CATALOG_CACHE: std::sync::OnceLock<Vec<SurveyDefinition>> = std::sync::OnceLock::new();

pub fn get_survey_catalog(sde: &eve_sde::SdeRegistry) -> &'static [SurveyDefinition] {
    SURVEY_CATALOG_CACHE.get_or_init(|| load_survey_catalog(sde))
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

/// Checks survey eligibility per V24 `surveys/survey.pyj`.
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

/// Builds a [`SurveyDefinition`] for `/survey schedule <survey_id> [duration_secs] [type_id:qty,...] [vgs_id,...]`.
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

/// Registers or overrides a survey instance in `OperationsStore`.
pub fn register_survey(ctx: &ServerContext, survey: SurveyDefinition) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        if let Some(existing) = store
            .custom_surveys
            .iter_mut()
            .find(|s| s.survey_id == survey.survey_id)
        {
            *existing = survey;
        } else {
            store.custom_surveys.push(survey);
        }
        save_store_pub(snap, &store, now);
    });
}

/// Marks an active or valid survey as completed for a character, enabling rewards claiming
/// and notifying the client via `OnSurveyRewardsAvailable`.
pub fn mark_survey_completed(
    ctx: &ServerContext,
    char_id: i64,
    survey_id: i64,
) -> Result<(), BridgeRpcError> {
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        if !snap.characters.contains_key(&char_id) {
            return Err(rpc_err("CharacterNotFound", "Character not found"));
        }
        let mut store = load_store_pub(snap);
        let catalog = combined_survey_catalog(&ctx.sde, &store.custom_surveys);
        let Some(survey) = catalog.iter().find(|s| s.survey_id == survey_id) else {
            return Err(rpc_err("InvalidSurveyID", format!("Invalid surveyID {survey_id}")));
        };
        if let Some(exp) = survey.expires_at_unix {
            if now > exp {
                return Err(rpc_err("SurveyExpired", format!("Survey {survey_id} has expired")));
            }
        }
        if let Some(start) = survey.start_unix {
            if now < start {
                return Err(rpc_err("SurveyNotActive", format!("Survey {survey_id} has not started yet")));
            }
        }
        let rec = store.by_character.entry(char_id).or_default();
        if rec.claimed_surveys.contains(&survey_id) {
            return Err(rpc_err("SurveyAlreadyClaimed", format!("Survey {survey_id} has already been claimed")));
        }
        rec.completed_surveys.insert(survey_id);
        save_store_pub(snap, &store, now);
        Ok(())
    })?;
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSurveyRewardsAvailable",
        "charid",
        vec![PyRep::Integer(survey_id)],
    );
    Ok(())
}

/// Handles GM/QA `/survey` slash commands (`schedule` / `activate` and `complete`).
pub fn handle_survey_slash(
    ctx: &ServerContext,
    caller_char_id: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let rest = cmd.strip_prefix("/survey")?.trim();
    let parts: Vec<&str> = rest.split_whitespace().collect();
    match parts.first().copied() {
        Some("schedule" | "activate") => match build_scheduled_survey_from_slash(&ctx.sde, ctx.now_unix(), &parts[1..]) {
            Ok(def) => {
                let sid = def.survey_id;
                let exp = def.expires_at_unix.unwrap_or(0) as i64;
                register_survey(ctx, def);
                Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("surveyID", PyRep::Integer(sid)),
                    ("scheduled", PyRep::Bool(true)),
                    ("expiresAtUnix", PyRep::Integer(exp)),
                ])))
            }
            Err(msg) => Some(Err(rpc_err("InvalidSlashArgument", msg))),
        },
        Some("complete") => {
            let target_cid = parts
                .get(2)
                .and_then(|s| s.parse::<i64>().ok())
                .filter(|&id| id > 0)
                .unwrap_or(caller_char_id);
            let sid = match parts.get(1).and_then(|s| s.parse::<i64>().ok()).filter(|&id| id > 0) {
                Some(id) => id,
                None => match ctx.db.with_snapshot(|snap| {
                    load_store_pub(snap)
                        .by_character
                        .get(&target_cid)
                        .and_then(|r| r.active_survey_id)
                }) {
                    Some(id) => id,
                    None => return Some(Err(BridgeRpcError::user_error("NoActiveSurvey"))),
                },
            };
            match mark_survey_completed(ctx, target_cid, sid) {
                Ok(()) => Some(Ok(PyRep::keyval([
                    ("command", PyRep::String(cmd.to_string())),
                    ("surveyID", PyRep::Integer(sid)),
                    ("characterID", PyRep::Integer(target_cid)),
                    ("completed", PyRep::Bool(true)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }
        _ => Some(Err(rpc_err(
            "InvalidSlashArgument",
            "Usage: /survey schedule <survey_id> [duration_secs] [type_id:qty,...] [vgs_id,...] | /survey complete [survey_id] [char_id]",
        ))),
    }
}

pub fn handle_survey_perform_checks(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let surveys = ctx.db.with_mut_snapshot(|snap| {
        let mut store = load_store_pub(snap);
        let catalog = combined_survey_catalog(&ctx.sde, &store.custom_surveys);
        let char_create_unix = snap
            .characters
            .get(&char_id)
            .map(|c| c.create_date_unix)
            .unwrap_or(0);
        let eligible = {
            let rec = store.by_character.entry(char_id).or_default();
            let list = eligible_surveys(rec, char_create_unix, &catalog, now);
            if rec.active_survey_id.is_none()
                || rec
                    .active_survey_id
                    .is_some_and(|id| !list.contains(&id) || rec.completed_surveys.contains(&id))
            {
                rec.active_survey_id = list.first().copied();
            }
            list
        };
        save_store_pub(snap, &store, now);
        eligible.into_iter().map(PyRep::Integer).collect()
    });
    Ok(PyRep::List(surveys))
}

pub fn handle_survey_claim_rewards(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (uid, char_id) = ServerContext::require_character(session)?;
    let explicit_survey_id = args.first().and_then(|a| a.as_i64());
    let now = ctx.now_unix();
    ctx.db.try_with_mut_snapshot(|snap| {
        let delivery_loc = snap
            .characters
            .get(&char_id)
            .ok_or_else(|| rpc_err("CharacterNotFound", "Character not found"))
            .map(|c| {
                c.station_id
                    .or(c.structure_id)
                    .or_else(|| (c.home_station_id > 0).then_some(c.home_station_id))
            })?;
        let mut store = load_store_pub(snap);
        let catalog = combined_survey_catalog(&ctx.sde, &store.custom_surveys);
        let item_rewards = {
            let rec = store.by_character.entry(char_id).or_default();
            let survey_id = if let Some(id) = explicit_survey_id {
                id
            } else if let Some(unclaimed_id) = rec
                .completed_surveys
                .iter()
                .find(|id| !rec.claimed_surveys.contains(id))
                .copied()
            {
                unclaimed_id
            } else if let Some(active_id) = rec.active_survey_id {
                active_id
            } else {
                return Err(BridgeRpcError::user_error("NoActiveSurvey"));
            };
            let Some(survey) = catalog.iter().find(|s| s.survey_id == survey_id) else {
                return Err(rpc_err("InvalidSurveyID", format!("Invalid surveyID {survey_id}")));
            };
            if let Some(exp) = survey.expires_at_unix {
                if now > exp {
                    return Err(rpc_err("SurveyExpired", format!("Survey {survey_id} has expired")));
                }
            }
            if let Some(start) = survey.start_unix {
                if now < start {
                    return Err(rpc_err("SurveyNotActive", format!("Survey {survey_id} has not started yet")));
                }
            }
            if rec.claimed_surveys.contains(&survey_id) {
                return Err(rpc_err("SurveyAlreadyClaimed", format!("Survey {survey_id} has already been claimed")));
            }
            if !rec.completed_surveys.contains(&survey_id) {
                return Err(rpc_err("SurveyNotCompleted", format!("Survey {survey_id} has not been completed yet")));
            }
            rec.claimed_surveys.insert(survey_id);
            if rec.active_survey_id == Some(survey_id) {
                rec.active_survey_id = None;
            }
            for &vgs_id in &survey.vgs_rewards {
                if !rec.claimed_vgs_rewards.contains(&vgs_id) {
                    rec.claimed_vgs_rewards.push(vgs_id);
                }
            }
            survey.item_rewards.clone()
        };
        save_store_pub(snap, &store, now);
        for (type_id, amount) in item_rewards {
            if amount == 0 {
                continue;
            }
            if let Some(loc_id) = delivery_loc {
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
            } else {
                crate::services::account_services::enqueue_redeem_token_in_snapshot(
                    snap,
                    uid,
                    type_id,
                    amount as i32,
                    "Survey reward".into(),
                    None,
                    None,
                );
            }
        }
        Ok(PyRep::Bool(true))
    })
}
