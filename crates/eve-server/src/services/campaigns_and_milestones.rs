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

/// Milestone definition with rewards and completion requirements.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MilestoneConfig {
    pub milestone_id: i64,
    pub name: &'static str,
    pub sp_reward: f64,
    pub isk_reward: i64,
    pub required_time_played_seconds: u64,
}

/// SYNTHETIC: invented milestone names, rewards and time requirements (no client/live source imported).
pub const MILESTONE_CONFIGS: &[MilestoneConfig] = &[
    MilestoneConfig {
        milestone_id: 1,
        name: "First Steps in New Eden",
        sp_reward: 5_000.0,
        isk_reward: 50_000,
        required_time_played_seconds: 0,
    },
    MilestoneConfig {
        milestone_id: 2,
        name: "Career Orientation",
        sp_reward: 10_000.0,
        isk_reward: 100_000,
        required_time_played_seconds: 60,
    },
    MilestoneConfig {
        milestone_id: 3,
        name: "Certified Capsuleer",
        sp_reward: 15_000.0,
        isk_reward: 150_000,
        required_time_played_seconds: 120,
    },
    MilestoneConfig {
        milestone_id: 4,
        name: "Market Competency",
        sp_reward: 20_000.0,
        isk_reward: 200_000,
        required_time_played_seconds: 300,
    },
    MilestoneConfig {
        milestone_id: 5,
        name: "Empire Pioneer",
        sp_reward: 25_000.0,
        isk_reward: 250_000,
        required_time_played_seconds: 600,
    },
];

pub fn get_milestone_config(milestone_id: i64) -> Option<&'static MilestoneConfig> {
    MILESTONE_CONFIGS.iter().find(|m| m.milestone_id == milestone_id)
}

/// SYNTHETIC: milestone IDs matching `MILESTONE_CONFIGS` above.
pub const VALID_MILESTONES: &[i64] = &[1, 2, 3, 4, 5];

/// Per-character limit on claimed milestones to prevent unbounded state growth.
pub const MAX_CLAIMED_MILESTONES_PER_CHAR: usize = 64;

/// FIFO buffer capacity for client telemetry events.
pub const MAX_TELEMETRY_EVENTS: usize = 1_000;

/// Proleptic Gregorian calendar date ordinal for Unix Epoch (1970-01-01).
/// V24 client `seasonalLoginCampaignService.pyj` uses `datetime.date.fromordinal(k)`.
pub const GREGORIAN_EPOCH_ORDINAL: i64 = 719_163;

/// Daily login campaign step reward configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DailyLoginRewardStep {
    pub day: i64,
    pub type_id: u32,
    pub quantity: i64,
    pub sp_reward: f64,
}

/// SYNTHETIC: invented 30-day login reward curve (no client/live source imported).
pub const DAILY_LOGIN_TRACK: &[DailyLoginRewardStep] = &[
    DailyLoginRewardStep { day: 1, type_id: 40_520, quantity: 1, sp_reward: 5_000.0 },
    DailyLoginRewardStep { day: 2, type_id: 40_520, quantity: 1, sp_reward: 5_000.0 },
    DailyLoginRewardStep { day: 3, type_id: 40_520, quantity: 1, sp_reward: 7_500.0 },
    DailyLoginRewardStep { day: 4, type_id: 40_520, quantity: 1, sp_reward: 7_500.0 },
    DailyLoginRewardStep { day: 5, type_id: 40_520, quantity: 1, sp_reward: 10_000.0 },
    DailyLoginRewardStep { day: 6, type_id: 40_520, quantity: 1, sp_reward: 10_000.0 },
    DailyLoginRewardStep { day: 7, type_id: 40_520, quantity: 1, sp_reward: 15_000.0 },
    DailyLoginRewardStep { day: 8, type_id: 40_520, quantity: 1, sp_reward: 15_000.0 },
    DailyLoginRewardStep { day: 9, type_id: 40_520, quantity: 1, sp_reward: 15_000.0 },
    DailyLoginRewardStep { day: 10, type_id: 40_520, quantity: 1, sp_reward: 20_000.0 },
    DailyLoginRewardStep { day: 11, type_id: 40_520, quantity: 1, sp_reward: 20_000.0 },
    DailyLoginRewardStep { day: 12, type_id: 40_520, quantity: 1, sp_reward: 20_000.0 },
    DailyLoginRewardStep { day: 13, type_id: 40_520, quantity: 1, sp_reward: 25_000.0 },
    DailyLoginRewardStep { day: 14, type_id: 40_520, quantity: 1, sp_reward: 25_000.0 },
    DailyLoginRewardStep { day: 15, type_id: 40_520, quantity: 1, sp_reward: 25_000.0 },
    DailyLoginRewardStep { day: 16, type_id: 40_520, quantity: 1, sp_reward: 25_000.0 },
    DailyLoginRewardStep { day: 17, type_id: 40_520, quantity: 1, sp_reward: 30_000.0 },
    DailyLoginRewardStep { day: 18, type_id: 40_520, quantity: 1, sp_reward: 30_000.0 },
    DailyLoginRewardStep { day: 19, type_id: 40_520, quantity: 1, sp_reward: 30_000.0 },
    DailyLoginRewardStep { day: 20, type_id: 40_520, quantity: 1, sp_reward: 35_000.0 },
    DailyLoginRewardStep { day: 21, type_id: 40_520, quantity: 1, sp_reward: 35_000.0 },
    DailyLoginRewardStep { day: 22, type_id: 40_520, quantity: 1, sp_reward: 35_000.0 },
    DailyLoginRewardStep { day: 23, type_id: 40_520, quantity: 1, sp_reward: 40_000.0 },
    DailyLoginRewardStep { day: 24, type_id: 40_520, quantity: 1, sp_reward: 40_000.0 },
    DailyLoginRewardStep { day: 25, type_id: 40_520, quantity: 1, sp_reward: 40_000.0 },
    DailyLoginRewardStep { day: 26, type_id: 40_520, quantity: 1, sp_reward: 45_000.0 },
    DailyLoginRewardStep { day: 27, type_id: 40_520, quantity: 1, sp_reward: 45_000.0 },
    DailyLoginRewardStep { day: 28, type_id: 40_520, quantity: 1, sp_reward: 50_000.0 },
    DailyLoginRewardStep { day: 29, type_id: 40_520, quantity: 1, sp_reward: 50_000.0 },
    DailyLoginRewardStep { day: 30, type_id: 40_520, quantity: 1, sp_reward: 75_000.0 },
];

pub fn get_daily_login_step(day: i64) -> DailyLoginRewardStep {
    DAILY_LOGIN_TRACK
        .iter()
        .find(|s| s.day == (day - 1).rem_euclid(DAILY_LOGIN_TRACK.len() as i64) + 1)
        .copied()
        .unwrap_or(DAILY_LOGIN_TRACK[0])
}

/// SYNTHETIC: seasonal campaign schedule entry. The real data lives in the client file
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`, which is not imported yet.
#[derive(Debug, Clone, PartialEq)]
pub struct SeasonalLoginSchedule {
    pub campaign_id: i64,
    pub name: &'static str,
    pub start_unix: u64,
    pub end_unix: u64,
    pub daily_sp_reward: f64,
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
        daily_sp_reward: 15_000.0,
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
            ("reward_count", PyRep::Integer(DAILY_LOGIN_TRACK.len() as i64)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(claimed_days.len() as i64)),
            ("total", PyRep::Integer(DAILY_LOGIN_TRACK.len() as i64)),
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

        let step = get_daily_login_step(reward_index);
        let ch = snap
            .characters
            .get_mut(&char_id)
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;
        ch.free_skill_points += step.sp_reward;
        let new_sp = ch.free_skill_points;
        let location_id = ch.station_id.unwrap_or(ch.home_station_id);

        let mut item_id_opt = None;
        if step.type_id > 0 && step.quantity > 0 {
            snap.next_entity_id += 1;
            let item_id = snap.next_entity_id;
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id: step.type_id,
                    owner_id: char_id,
                    location_id,
                    flag: eve_db::InventoryFlag::Hangar,
                    quantity: step.quantity as i32,
                    singleton: false,
                    version: 1,
                    dynamic_attributes: std::collections::HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            item_id_opt = Some(item_id);
        }
        save_cosmetics_ui_state(snap, &ext);

        let mut reward_fields = vec![
            ("campaign_id", PyRep::Integer(101)),
            ("campaignID", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
            ("typeID", PyRep::Integer(step.type_id as i64)),
            ("quantity", PyRep::Integer(step.quantity)),
            ("sp_granted", PyRep::Real(step.sp_reward)),
            ("spGranted", PyRep::Real(step.sp_reward)),
            ("free_skill_points", PyRep::Real(new_sp)),
            ("freeSkillPoints", PyRep::Real(new_sp)),
        ];
        if let Some(item_id) = item_id_opt {
            reward_fields.push(("item_id", PyRep::Integer(item_id)));
            reward_fields.push(("itemID", PyRep::Integer(item_id)));
        }
        let reward_item = PyRep::keyval(reward_fields);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(reward_index + 1)),
            ("reward_count", PyRep::Integer(DAILY_LOGIN_TRACK.len() as i64)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
        ]);

        let reward_event = PyRep::keyval([
            ("event_id", PyRep::Integer(101)),
            ("reward_index", PyRep::Integer(reward_index)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(reward_index)),
            ("total", PyRep::Integer(DAILY_LOGIN_TRACK.len() as i64)),
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
            let (campaign_id, daily_sp) = (sched.campaign_id, sched.daily_sp_reward);
            let date_ordinal = GREGORIAN_EPOCH_ORDINAL + (day_unix as i64);
            let claim_data = PyRep::keyval([
                ("date_ordinal", PyRep::Integer(date_ordinal)),
                ("campaign_id", PyRep::Integer(campaign_id)),
                ("claimed_timestamp", PyRep::Integer((day_unix * 86_400) as i64)),
                ("sp_granted", PyRep::Real(daily_sp)),
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
    let s = get_active_seasonal_campaign(now).ok_or_else(|| {
        err("NoActiveCampaign", "No seasonal campaign is currently active")
    })?;
    let daily_sp = s.daily_sp_reward;

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
        ch.free_skill_points += daily_sp;
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

    let config = get_milestone_config(milestone_id).ok_or_else(|| {
        err(
            "InvalidMilestoneID",
            format!("Milestone {milestone_id} is not in recognized milestone catalog"),
        )
    })?;

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);

        // Prerequisite check: character must have logged on via ProcessCharacterLogon.
        // NOTE: "time played" is the time since the most recent ProcessCharacterLogon (session time),
        // not accumulated play time; the milestone rows stay open.
        let logon_time = ext.milestone_logons.get(&char_id).copied().ok_or_else(|| {
            err(
                "MilestonePrerequisiteNotMet",
                "Character must complete logon processing before claiming milestones",
            )
        })?;

        let time_played = now.saturating_sub(logon_time);
        if time_played < config.required_time_played_seconds {
            return Err(err(
                "MilestoneEligibilityNotMet",
                format!(
                    "Milestone {milestone_id} requires at least {}s activity (current: {time_played}s)",
                    config.required_time_played_seconds
                ),
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

        ch.free_skill_points += config.sp_reward;
        let reward_cents = eve_db::IskCents::from_isk(config.isk_reward);
        ch.balance = ch.balance.checked_add(reward_cents).unwrap_or(ch.balance);

        snap.next_entity_id += 1;
        let jid = snap.next_entity_id;
        snap.wallet_journal.push(eve_db::WalletJournalEntry {
            entry_id: jid,
            character_id: char_id,
            entry_type: "milestone_reward".to_string(),
            amount: reward_cents,
            balance_after: ch.balance,
            reason: format!("Milestone {milestone_id} ({}) completion reward", config.name),
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
