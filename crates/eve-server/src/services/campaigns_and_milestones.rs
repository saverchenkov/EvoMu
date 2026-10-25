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

/// Bounded catalog of recognized milestone IDs.
pub const VALID_MILESTONES: &[i64] = &[1, 2, 3, 4, 5];

/// Per-character limit on claimed milestones to prevent unbounded state growth.
pub const MAX_CLAIMED_MILESTONES_PER_CHAR: usize = 64;

/// FIFO buffer capacity for client telemetry events.
pub const MAX_TELEMETRY_EVENTS: usize = 1_000;

/// Proleptic Gregorian calendar date ordinal for Unix Epoch (1970-01-01).
/// V24 client `seasonalLoginCampaignService.pyj` uses `datetime.date.fromordinal(k)`.
pub const GREGORIAN_EPOCH_ORDINAL: i64 = 719_163;

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
            Some(handle_info_gathering_log_events(ctx, session, args))
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
    ServerContext::require_auth(session)?;

    // Character-selection screen before character selected: no campaign state
    let Some(char_id) = session.character_id else {
        return Ok(PyRep::None);
    };

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
    ServerContext::require_auth(session)?;

    // Character-selection screen before character selected
    if session.character_id.is_none() {
        return Ok(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::None,
            PyRep::None,
            PyRep::None,
        ]));
    }

    let now = ctx.now_unix();
    let unix_days = (now / 86_400) as i64;
    let today_ordinal = GREGORIAN_EPOCH_ORDINAL + unix_days;
    let start_date_ordinal = today_ordinal - 2;

    let now_sec = now as i64;
    let end_unix = (unix_days + 14) * 86_400;
    let days_remaining = if end_unix > now_sec {
        (end_unix - now_sec + 86_399) / 86_400
    } else {
        0
    };
    let end_date_str = "2026-11-15 11:00:00";
    let active_campaign_id = 202i64;

    Ok(PyRep::Tuple(vec![
        PyRep::Integer(active_campaign_id),
        PyRep::Integer(start_date_ordinal),
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
        for &day_unix in &claimed_days {
            let date_ordinal = GREGORIAN_EPOCH_ORDINAL + (day_unix as i64);
            let claim_data = PyRep::keyval([
                ("date_ordinal", PyRep::Integer(date_ordinal)),
                ("campaign_id", PyRep::Integer(202)),
                ("claimed_timestamp", PyRep::Integer((day_unix * 86_400) as i64)),
                ("sp_granted", PyRep::Real(15_000.0)),
            ]);
            entries.push((PyRep::Integer(date_ordinal), claim_data));
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
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let events = args
        .first()
        .ok_or_else(|| err("InvalidArgument", "Missing events dictionary or list"))?;

    let (status_entries, event_logs): (Vec<(PyRep, PyRep)>, Vec<String>) = match events {
        PyRep::Dict(entries) => {
            let mut status = Vec::with_capacity(entries.len());
            let mut logs = Vec::with_capacity(entries.len());
            for (k, v) in entries {
                let key_str = match k {
                    PyRep::String(s) => s.clone(),
                    PyRep::Integer(i) => i.to_string(),
                    _ => "unknown".to_string(),
                };
                logs.push(format!("{key_str}:{v:?}"));
                status.push((k.clone(), PyRep::Integer(0)));
            }
            (status, logs)
        }
        PyRep::List(items) => {
            let mut status = Vec::with_capacity(items.len());
            let mut logs = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let key = match item {
                    PyRep::Integer(id) => PyRep::Integer(*id),
                    PyRep::String(s) => PyRep::String(s.clone()),
                    _ => PyRep::Integer(idx as i64),
                };
                logs.push(format!("{item:?}"));
                status.push((key, PyRep::Integer(0)));
            }
            (status, logs)
        }
        _ => {
            return Err(err(
                "InvalidArgument",
                "LogInfoEventsFromClient requires Dict or List of events",
            ))
        }
    };

    // Record in bounded FIFO telemetry buffer in snapshot
    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.client_telemetry_events.extend(event_logs);
        if ext.client_telemetry_events.len() > MAX_TELEMETRY_EVENTS {
            let overflow = ext.client_telemetry_events.len() - MAX_TELEMETRY_EVENTS;
            ext.client_telemetry_events.drain(0..overflow);
        }
        save_cosmetics_ui_state(snap, &ext);
    });

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
    let now = ctx.now_unix();

    let milestone_id = args
        .first()
        .and_then(|v| v.as_i64())
        .ok_or_else(|| err("InvalidArgument", "ClaimRewards requires milestone_id integer"))?;

    if milestone_id <= 0 || !VALID_MILESTONES.contains(&milestone_id) {
        return Err(err(
            "InvalidMilestoneID",
            format!("Milestone {milestone_id} is not in recognized milestone catalog"),
        ));
    }

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);

        // Prerequisite check: character must have logged on via ProcessCharacterLogon
        if !ext.milestone_logons.contains_key(&char_id) {
            return Err(err(
                "MilestonePrerequisiteNotMet",
                "Character must complete logon processing before claiming milestones",
            ));
        }

        let claimed = ext.claimed_milestones.entry(char_id).or_default();
        if claimed.contains(&milestone_id) {
            return Err(err(
                "MilestoneAlreadyClaimed",
                format!("Milestone {milestone_id} has already been claimed"),
            ));
        }

        if claimed.len() >= MAX_CLAIMED_MILESTONES_PER_CHAR {
            return Err(err(
                "MilestoneLimitReached",
                "Character has reached the maximum claimed milestone limit",
            ));
        }

        claimed.push(milestone_id);
        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;

        ch.free_skill_points += 10_000.0;
        let reward_cents = eve_db::IskCents::from_isk(100_000);
        ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);

        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "milestone_reward".to_string(),
            amount: reward_cents,
            balance_after: ch.balance,
            reason: format!("Milestone {milestone_id} completion reward"),
        });
        snap.wallet_journal_timestamps.insert(jid, now);

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
        let progress = ext
            .community_track_points
            .get(&instance_id)
            .copied()
            .unwrap_or(50_000);

        // Canonical 2-tuple (fsd_identifier, progress) per V24 communitytracks/client/repository.pyj:114
        Ok(PyRep::Tuple(vec![
            PyRep::Integer(instance_id),
            PyRep::Integer(progress),
        ]))
    })
}

fn handle_community_track_change_qa_mode(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;

    let has_qa_perm = session.has_gm_role()
        || (session.role
            & (eve_machonet::service_roles::ROLE_QA
                | eve_machonet::service_roles::ROLE_PROGRAMMER))
            != 0;

    if !has_qa_perm {
        return Err(err(
            "AccessDenied",
            "communityTrackMgr.change_qa_mode requires QA or GM privileges",
        ));
    }

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
        // Update track progress for QA test verification only when QA mode is enabled
        if qa_mode {
            ext.community_track_points.insert(1001, 75_000);
        }
        save_cosmetics_ui_state(snap, &ext);
    });

    Ok(PyRep::Bool(true))
}
