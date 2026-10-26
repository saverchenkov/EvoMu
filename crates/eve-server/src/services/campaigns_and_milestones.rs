//! Campaigns, Milestones, Community Tracks, and Telemetry RPC Service Handlers.
//!
//! Implements the V24 client RPC surface for the services below. All reward/schedule tables in this
//! module are SYNTHETIC placeholders (not imported from client or live data); see each table's note.
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

/// SYNTHETIC: milestone IDs accepted by `milestoneMgr.ClaimRewards`. The real milestone catalogue and
/// rewards are not imported, so claiming only records the claim and grants nothing.
pub const VALID_MILESTONES: &[i64] = &[1, 2, 3, 4, 5];

/// Per-character limit on claimed milestones to prevent unbounded state growth.
pub const MAX_CLAIMED_MILESTONES_PER_CHAR: usize = 64;

/// FIFO buffer capacity for client telemetry events.
pub const MAX_TELEMETRY_EVENTS: usize = 1_000;

/// Proleptic Gregorian calendar date ordinal for Unix Epoch (1970-01-01).
/// V24 client `seasonalLoginCampaignService.pyj` uses `datetime.date.fromordinal(k)`.
pub const GREGORIAN_EPOCH_ORDINAL: i64 = 719_163;

/// SYNTHETIC: length of the daily login track. The real login campaign rewards are not imported, so
/// `loginCampaignManager.claim_reward` only records the claim and delivers no items, SP or ISK.
pub const LOGIN_TRACK_LENGTH: i64 = 30;

/// SYNTHETIC: seasonal campaign schedule entry. The real data lives in the client file
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`, which is not imported yet.
#[derive(Debug, Clone, PartialEq)]
pub struct SeasonalLoginSchedule {
    pub campaign_id: i64,
    pub name: &'static str,
    pub start_unix: u64,
    pub end_unix: u64,
}

impl SeasonalLoginSchedule {
    /// End date as `YYYY-MM-DD HH:MM:SS` (UTC), derived from `end_unix` so there is one source of truth.
    pub fn end_date_str(&self) -> String {
        format_unix_utc(self.end_unix)
    }

    fn contains(&self, unix: u64) -> bool {
        unix >= self.start_unix && unix < self.end_unix
    }
}

/// Formats a Unix timestamp as `YYYY-MM-DD HH:MM:SS` UTC (proleptic Gregorian, civil-from-days).
pub fn format_unix_utc(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let secs = unix % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

pub const SEASONAL_LOGIN_SCHEDULES: &[SeasonalLoginSchedule] = &[
    SeasonalLoginSchedule {
        campaign_id: 202,
        name: "Capsuleer Day Celebration",
        start_unix: 1_700_000_000,
        end_unix: 1_850_000_000,
    },
];

pub fn get_active_seasonal_campaign(now_unix: u64) -> Option<&'static SeasonalLoginSchedule> {
    SEASONAL_LOGIN_SCHEDULES
        .iter()
        .find(|s| s.contains(now_unix))
}

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
            ("reward_count", PyRep::Integer(LOGIN_TRACK_LENGTH)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(claimed_days.len() as i64)),
            ("total", PyRep::Integer(LOGIN_TRACK_LENGTH)),
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

        if !snap.characters.contains_key(&char_id) {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        }
        save_cosmetics_ui_state(snap, &ext);

        // Non-delivering: the real reward table is not imported, so no item, SP or ISK is granted.
        let reward_item = PyRep::keyval([
            ("campaign_id", PyRep::Integer(101)),
            ("campaignID", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(reward_index + 1)),
            ("reward_count", PyRep::Integer(LOGIN_TRACK_LENGTH)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let reward_event = PyRep::keyval([
            ("event_id", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(reward_index)),
            ("total", PyRep::Integer(LOGIN_TRACK_LENGTH)),
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
    let s = get_active_seasonal_campaign(now).ok_or_else(|| {
        err("NoActiveCampaign", "No seasonal campaign is currently active")
    })?;

    let start_date_ordinal = GREGORIAN_EPOCH_ORDINAL + (s.start_unix / 86_400) as i64;
    let days_remaining = (s.end_unix.saturating_sub(now) + 86_399) / 86_400;

    Ok(PyRep::Tuple(vec![
        PyRep::Integer(s.campaign_id),
        PyRep::Integer(start_date_ordinal),
        PyRep::String(s.end_date_str()),
        PyRep::Integer(days_remaining as i64),
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
            // Attribute each claim to the schedule whose window contains the claim day; claims that
            // fall outside every known schedule are skipped instead of inventing a campaign.
            let Some(sched) = SEASONAL_LOGIN_SCHEDULES.iter().find(|s| s.contains(day_unix * 86_400))
            else {
                continue;
            };
            let campaign_id = sched.campaign_id;
            let date_ordinal = GREGORIAN_EPOCH_ORDINAL + (day_unix as i64);
            let claim_data = PyRep::keyval([
                ("date_ordinal", PyRep::Integer(date_ordinal)),
                ("campaign_id", PyRep::Integer(campaign_id)),
                ("claimed_timestamp", PyRep::Integer((day_unix * 86_400) as i64)),
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
    let now = ctx.now_unix();
    let today = now / 86_400;
    get_active_seasonal_campaign(now).ok_or_else(|| {
        err("NoActiveCampaign", "No seasonal campaign is currently active")
    })?;

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
        if !snap.characters.contains_key(&char_id) {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        }
        // Non-delivering: the seasonal reward data is not imported, so nothing is granted.
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

    let milestone_id = args
        .first()
        .and_then(|v| v.as_i64())
        .ok_or_else(|| err("InvalidArgument", "ClaimRewards requires milestone_id integer"))?;

    if !VALID_MILESTONES.contains(&milestone_id) {
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
        if !snap.characters.contains_key(&char_id) {
            return Err(err("CharacterNotFound", format!("Character {char_id} not found")));
        }
        // Non-delivering: the real milestone rewards are not imported, so no SP or ISK is granted.
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
            .unwrap_or(0);

        // 2-tuple (fsd_identifier, progress) per V24 communitytracks/client/repository.pyj:114;
        // a track with no recorded contributions reports 0 progress.
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

    let mode_rep = args
        .first()
        .ok_or_else(|| err("InvalidArgument", "change_qa_mode requires 1 argument (mode)"))?;

    let qa_mode = match mode_rep {
        PyRep::Bool(b) => *b,
        PyRep::Integer(i) => *i != 0,
        _ => return Err(err("InvalidArgument", "mode must be a boolean or integer")),
    };

    ctx.db.with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        ext.community_track_qa_mode = qa_mode;
        save_cosmetics_ui_state(snap, &ext);
    });

    Ok(PyRep::Bool(true))
}
