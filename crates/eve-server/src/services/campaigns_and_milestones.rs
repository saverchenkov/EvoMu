//! Campaigns, Milestones, Community Tracks, and Telemetry RPC Service Handlers.
//!
//! Provides canonical V24 live-server implementations for:
//! - `loginCampaignManager.get_client_campaign_state` and `loginCampaignManager.claim_reward`
//! - `seasonalLoginCampaignManager.get_active_campaign`, `seasonalLoginCampaignManager.get_claim_history`,
//!   and `seasonalLoginCampaignManager.claim_reward`
//! - `infoGatheringMgr.GetStateAndConfig` and `infoGatheringMgr.LogInfoEventsFromClient`
//! - `milestoneMgr.ProcessCharacterLogon` and `milestoneMgr.ClaimRewards`
//! - `communityTrackMgr.get_instance_attributes_public` and `communityTrackMgr.change_qa_mode`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::ServerContext;
use crate::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, save_cosmetics_ui_state,
};

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("loginCampaignManager", "get_client_campaign_state") => {
            Some(handle_login_campaign_get_state(ctx, session))
        }
        ("loginCampaignManager", "claim_reward") => {
            Some(handle_login_campaign_claim_reward(ctx, session))
        }
        ("seasonalLoginCampaignManager", "get_active_campaign") => {
            Some(handle_seasonal_login_get_active_campaign(ctx, session))
        }
        ("seasonalLoginCampaignManager", "get_claim_history") => {
            Some(handle_seasonal_login_get_claim_history(ctx, session))
        }
        ("seasonalLoginCampaignManager", "claim_reward") => {
            Some(handle_seasonal_login_claim_reward(ctx, session))
        }
        ("infoGatheringMgr", "GetStateAndConfig") => {
            Some(handle_info_gathering_get_state_and_config(session))
        }
        ("infoGatheringMgr", "LogInfoEventsFromClient") => {
            Some(handle_info_gathering_log_events(session, args))
        }
        ("milestoneMgr", "ProcessCharacterLogon") => {
            Some(handle_milestone_process_logon(ctx, session))
        }
        ("milestoneMgr", "ClaimRewards") => {
            Some(handle_milestone_claim_rewards(ctx, session, args))
        }
        ("communityTrackMgr", "get_instance_attributes_public") => {
            Some(handle_community_track_get_attributes(ctx, session, args))
        }
        ("communityTrackMgr", "change_qa_mode") => {
            Some(handle_community_track_change_qa_mode(ctx, session, args))
        }
        _ => None,
    }
}

fn handle_login_campaign_get_state(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext
            .login_campaign_claimed_days
            .get(&char_id)
            .cloned()
            .unwrap_or_default();
        let already_claimed_today = claimed_days.contains(&today);
        let next_reward_idx = (claimed_days.len() as i64) + 1;

        let claimed_reps: Vec<PyRep> = (1..=claimed_days.len() as i64)
            .map(PyRep::Integer)
            .collect();

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(next_reward_idx)),
            ("reward_count", PyRep::Integer(30)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(claimed_days.len() as i64)),
            ("total", PyRep::Integer(30)),
        ]);

        Ok(PyRep::keyval([
            ("campaign_id", PyRep::Integer(101)),
            ("campaignID", PyRep::Integer(101)),
            ("item_progress", item_progress),
            ("bucket_progress", bucket_progress),
            ("reward_event", PyRep::None),
            ("can_claim_today", PyRep::Bool(!already_claimed_today)),
            ("canClaimToday", PyRep::Bool(!already_claimed_today)),
            ("current_day", PyRep::Integer(today as i64)),
            ("currentDay", PyRep::Integer(today as i64)),
            ("total_days_claimed", PyRep::Integer(claimed_days.len() as i64)),
            ("totalDaysClaimed", PyRep::Integer(claimed_days.len() as i64)),
        ]))
    })
}

fn handle_login_campaign_claim_reward(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.login_campaign_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Err(err(
                "RewardAlreadyClaimedToday",
                "Daily login reward has already been claimed for today",
            ));
        }

        claimed_days.push(today);
        let reward_index = claimed_days.len() as i64;
        let claimed_reps: Vec<PyRep> = (1..=reward_index).map(PyRep::Integer).collect();

        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 5_000.0;
        let new_sp = ch.free_skill_points;
        save_cosmetics_ui_state(snap, &ext);

        let reward_item = PyRep::keyval([
            ("campaign_id", PyRep::Integer(101)),
            ("campaignID", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
            ("typeID", PyRep::Integer(40_520)),
            ("quantity", PyRep::Integer(1)),
            ("sp_granted", PyRep::Real(5_000.0)),
            ("spGranted", PyRep::Real(5_000.0)),
            ("free_skill_points", PyRep::Real(new_sp)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(reward_index + 1)),
            ("reward_count", PyRep::Integer(30)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let reward_event = PyRep::keyval([
            ("event_id", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(reward_index)),
            ("total", PyRep::Integer(30)),
        ]);

        Ok(PyRep::Tuple(vec![
            reward_item,
            item_progress,
            reward_event,
            bucket_progress,
        ]))
    })
}

fn handle_seasonal_login_get_active_campaign(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, _char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let today_ordinal = (now / 86_400) as i64;
    let start_ordinal = today_ordinal - 2;
    let end_date_str = "2026-11-15 11:00:00";
    let days_remaining = 14i64;
    let active_campaign_id = 202i64;

    Ok(PyRep::Tuple(vec![
        PyRep::Integer(active_campaign_id),
        PyRep::Integer(start_ordinal),
        PyRep::String(end_date_str.into()),
        PyRep::Integer(days_remaining),
    ]))
}

fn handle_seasonal_login_get_claim_history(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext
            .seasonal_claimed_days
            .get(&char_id)
            .cloned()
            .unwrap_or_default();

        let mut entries = Vec::with_capacity(claimed_days.len());
        for &day_ordinal in &claimed_days {
            let claim_data = PyRep::keyval([
                ("date_ordinal", PyRep::Integer(day_ordinal as i64)),
                ("campaign_id", PyRep::Integer(202)),
                ("claimed_timestamp", PyRep::Integer((day_ordinal * 86_400) as i64)),
                ("sp_granted", PyRep::Real(15_000.0)),
            ]);
            entries.push((PyRep::Integer(day_ordinal as i64), claim_data));
        }
        Ok(PyRep::Dict(entries))
    })
}

fn handle_seasonal_login_claim_reward(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed_days = ext.seasonal_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Err(err(
                "SeasonalRewardAlreadyClaimedToday",
                "Seasonal reward already claimed for today",
            ));
        }

        claimed_days.push(today);
        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 15_000.0;
        save_cosmetics_ui_state(snap, &ext);

        Ok(PyRep::Bool(true))
    })
}

fn handle_info_gathering_get_state_and_config(
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    Ok(PyRep::keyval([
        ("clientWorkerInterval", PyRep::Integer(300)),
        ("infoTypeAggregates", PyRep::Dict(Vec::new())),
        ("infoTypeParameters", PyRep::Dict(Vec::new())),
        ("infoTypes", PyRep::List(Vec::new())),
        ("infoTypesOncePerRun", PyRep::List(Vec::new())),
        ("isEnabled", PyRep::Bool(true)),
    ]))
}

fn handle_info_gathering_log_events(
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let events = args
        .first()
        .ok_or_else(|| err("InvalidArgument", "Missing events dictionary or list"))?;

    let status_entries = match events {
        PyRep::Dict(entries) => entries
            .iter()
            .map(|(k, _)| (k.clone(), PyRep::Integer(0)))
            .collect(),
        PyRep::List(items) => items
            .iter()
            .enumerate()
            .map(|(idx, item)| {
                let key = match item {
                    PyRep::Integer(id) => PyRep::Integer(*id),
                    PyRep::String(s) => PyRep::String(s.clone()),
                    _ => PyRep::Integer(idx as i64),
                };
                (key, PyRep::Integer(0))
            })
            .collect(),
        _ => {
            return Err(err(
                "InvalidArgument",
                "LogInfoEventsFromClient requires Dict or List of events",
            ))
        }
    };

    Ok(PyRep::Dict(status_entries))
}

fn handle_milestone_process_logon(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.milestone_logons.insert(char_id, now);
        save_cosmetics_ui_state(snap, &ext);
    });

    Ok(PyRep::None)
}

fn handle_milestone_claim_rewards(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;

    let milestone_id = args
        .first()
        .and_then(|v| v.as_i64())
        .ok_or_else(|| err("InvalidArgument", "ClaimRewards requires milestone_id integer"))?;

    if milestone_id <= 0 {
        return Err(err(
            "InvalidMilestoneID",
            format!("Milestone {milestone_id} is invalid"),
        ));
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let claimed = ext.claimed_milestones.entry(char_id).or_default();
        if claimed.contains(&milestone_id) {
            return Err(err(
                "MilestoneAlreadyClaimed",
                format!("Milestone {milestone_id} has already been claimed"),
            ));
        }

        claimed.push(milestone_id);
        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += 10_000.0;
        ch.balance = ch
            .balance
            .checked_add(eve_db::IskCents::from_isk(100_000))
            .unwrap_or(ch.balance);
        save_cosmetics_ui_state(snap, &ext);

        Ok(PyRep::None)
    })
}

fn handle_community_track_get_attributes(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let instance_id = args.first().and_then(|v| v.as_i64()).ok_or_else(|| {
        err(
            "InvalidArgument",
            "get_instance_attributes_public requires instance_id integer",
        )
    })?;

    if instance_id <= 0 {
        return Err(err(
            "InvalidInstanceID",
            format!("Instance ID {instance_id} is invalid"),
        ));
    }

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let points = ext
            .community_track_points
            .get(&instance_id)
            .copied()
            .unwrap_or(50_000);

        let def_id = PyRep::keyval([
            ("definition_id", PyRep::Integer(instance_id)),
            ("track_id", PyRep::Integer(instance_id)),
            ("name", PyRep::String("Community Track Event".into())),
        ]);

        let attrs = PyRep::keyval([
            ("instance_id", PyRep::Integer(instance_id)),
            ("points", PyRep::Integer(points)),
            ("target_points", PyRep::Integer(100_000)),
            ("tier", PyRep::Integer(1)),
            ("is_active", PyRep::Bool(true)),
        ]);

        Ok(PyRep::Tuple(vec![def_id, attrs]))
    })
}

fn handle_community_track_change_qa_mode(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let qa_mode = match args.first() {
        Some(PyRep::Bool(b)) => *b,
        Some(PyRep::Integer(i)) => *i != 0,
        _ => {
            return Err(err(
                "InvalidArgument",
                "change_qa_mode requires boolean or integer",
            ))
        }
    };

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.community_track_qa_mode = qa_mode;
        save_cosmetics_ui_state(snap, &ext);
    });

    Ok(PyRep::Bool(true))
}
