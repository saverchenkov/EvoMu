//! Campaigns, Milestones, Community Tracks, and Telemetry RPC Service Handlers.
//!
//! Implements the V24 client RPC surface for:
//! - `loginCampaignManager.get_client_campaign_state` and `loginCampaignManager.claim_reward`
//!   (backed by `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`)
//! - `seasonalLoginCampaignManager.get_active_campaign`, `seasonalLoginCampaignManager.get_claim_history`,
//!   and `seasonalLoginCampaignManager.claim_reward`
//!   (backed by `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`)
//! - `infoGatheringMgr.GetStateAndConfig` and `infoGatheringMgr.LogInfoEventsFromClient`
//! - `milestoneMgr.ProcessCharacterLogon` and `milestoneMgr.ClaimRewards`
//!   (`DATA GAP`: milestone reward definitions are server-authored and not in client FSD data)
//! - `communityTrackMgr.get_instance_attributes_public` and `communityTrackMgr.change_qa_mode`
//!   (backed by `res:/staticdata/community_track_templates.fsdbinary`)

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

use crate::router::ServerContext;
use crate::services::cosmetics_ui_ops::{
    load_cosmetics_ui_state, save_cosmetics_ui_state,
};

/// FIFO buffer capacity for client telemetry events.
pub const MAX_TELEMETRY_EVENTS: usize = 1_000;

/// Proleptic Gregorian calendar date ordinal for Unix Epoch (1970-01-01).
/// V24 client `seasonalLoginCampaignService.pyj` uses `datetime.date.fromordinal(k)`.
pub const GREGORIAN_EPOCH_ORDINAL: i64 = 719_163;

/// Default daily login campaign ID from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`
/// (campaign `2261`: Capsuleer Day XXI, 30-day track with Alpha and Omega item rewards).
pub const DEFAULT_LOGIN_CAMPAIGN_ID: i64 = 2261;

/// Default seasonal login campaign ID from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`
/// (campaign `2323`: Capsuleer Day XXII, 30-day track with Alpha and Omega item rewards).
pub const DEFAULT_SEASONAL_CAMPAIGN_ID: i64 = 2323;

/// Reward entry decoded from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeLoginRewardEntry {
    pub type_id: u32,
    pub quantity: u32,
    pub tier: u32,
    pub blueprint_runs: Option<u32>,
    pub blueprint_material_level: Option<u32>,
    pub blueprint_productivity_level: Option<u32>,
    pub redeemed_expiry_days: Option<u32>,
    pub is_auto_injected_booster_or_skin: bool,
    pub overlay_texture_path: Option<String>,
}

/// Campaign definition decoded from `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeLoginCampaign {
    pub campaign_id: i64,
    pub campaign_name_id: u32,
    pub auto_claim: bool,
    pub retro_claim: bool,
    pub window_header_background: Option<String>,
    pub alpha_rewards: BTreeMap<u32, SdeLoginRewardEntry>,
    pub omega_rewards: BTreeMap<u32, SdeLoginRewardEntry>,
}

impl SdeLoginCampaign {
    /// Total reward track length derived from the SDE campaign's reward tables.
    pub fn track_length(&self) -> i64 {
        self.alpha_rewards.len().max(self.omega_rewards.len()) as i64
    }
}

/// Community track template decoded from `res:/staticdata/community_track_templates.fsdbinary`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SdeCommunityTrackTemplate {
    pub template_id: i64,
    pub name: String,
    pub goal: u32,
    pub tale_templates: Vec<u32>,
}

static SDE_LOGIN_CAMPAIGNS: OnceLock<BTreeMap<i64, SdeLoginCampaign>> = OnceLock::new();
static SDE_COMMUNITY_TRACKS: OnceLock<BTreeMap<i64, SdeCommunityTrackTemplate>> = OnceLock::new();

fn parse_reward_map(val: Option<&serde_json::Value>) -> BTreeMap<u32, SdeLoginRewardEntry> {
    let mut out = BTreeMap::new();
    let Some(obj) = val.and_then(|v| v.as_object()) else {
        return out;
    };
    for (day_str, r) in obj {
        let Ok(day_idx) = day_str.parse::<u32>() else {
            continue;
        };
        let Some(type_id) = r.get("typeID").and_then(|v| v.as_u64()).map(|v| v as u32) else {
            continue;
        };
        let quantity = r
            .get("quantity")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(1)
            .max(1);
        let tier = r
            .get("tier")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(0);
        out.insert(
            day_idx,
            SdeLoginRewardEntry {
                type_id,
                quantity,
                tier,
                blueprint_runs: r.get("blueprintRuns").and_then(|v| v.as_u64()).map(|v| v as u32),
                blueprint_material_level: r
                    .get("blueprintMaterialLevel")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                blueprint_productivity_level: r
                    .get("blueprintProductivityLevel")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                redeemed_expiry_days: r
                    .get("redeemedExpiryDays")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32),
                is_auto_injected_booster_or_skin: r
                    .get("isAutoInjectedBoosterOrSkin")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0)
                    != 0,
                overlay_texture_path: r
                    .get("overlayTexturePath")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            },
        );
    }
    out
}

pub fn get_sde_login_campaigns(sde: &eve_sde::SdeRegistry) -> &'static BTreeMap<i64, SdeLoginCampaign> {
    SDE_LOGIN_CAMPAIGNS.get_or_init(|| {
        let mut map = BTreeMap::new();
        if let Ok(raw) = sde.load_cfsd_nested("res:/staticdata/seasonalcampaignsloginrewards.fsdbinary") {
            for (cid_str, val) in raw {
                let Ok(campaign_id) = cid_str.parse::<i64>() else {
                    continue;
                };
                let campaign_name_id = val
                    .get("campaignName")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(0);
                let auto_claim = val.get("autoClaim").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
                let retro_claim = val.get("retroClaim").and_then(|v| v.as_u64()).unwrap_or(0) != 0;
                let window_header_background = val
                    .get("windowHeaderBackground")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                let alpha_rewards = parse_reward_map(val.get("alphaRewards"));
                let omega_rewards = parse_reward_map(val.get("omegaRewards"));
                map.insert(
                    campaign_id,
                    SdeLoginCampaign {
                        campaign_id,
                        campaign_name_id,
                        auto_claim,
                        retro_claim,
                        window_header_background,
                        alpha_rewards,
                        omega_rewards,
                    },
                );
            }
        }
        map
    })
}

pub fn get_sde_community_tracks(
    sde: &eve_sde::SdeRegistry,
) -> &'static BTreeMap<i64, SdeCommunityTrackTemplate> {
    SDE_COMMUNITY_TRACKS.get_or_init(|| {
        let mut map = BTreeMap::new();
        if let Ok(raw) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
            for (tid_str, val) in raw {
                let Ok(template_id) = tid_str.parse::<i64>() else {
                    continue;
                };
                let name = val
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let goal = val
                    .get("goal")
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .unwrap_or(100);
                let tale_templates = val
                    .get("tale_templates")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|x| x.as_u64().map(|n| n as u32))
                            .collect()
                    })
                    .unwrap_or_default();
                map.insert(
                    template_id,
                    SdeCommunityTrackTemplate {
                        template_id,
                        name,
                        goal,
                        tale_templates,
                    },
                );
            }
        }
        map
    })
}

/// Seasonal campaign schedule window referencing an SDE campaign ID in
/// `res:/staticdata/seasonalcampaignsloginrewards.fsdbinary`.
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
        campaign_id: DEFAULT_SEASONAL_CAMPAIGN_ID,
        name: "Capsuleer Day XXII",
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

fn deliver_reward_entry_to_hangar(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    home_station_id: i64,
    rw: &SdeLoginRewardEntry,
) -> i64 {
    snap.next_entity_id += 1;
    let item_id = snap.next_entity_id;
    snap.items.insert(
        item_id,
        eve_db::InventoryItemRecord {
            item_id,
            type_id: rw.type_id,
            owner_id: char_id,
            location_id: home_station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: rw.quantity.max(1) as i32,
            singleton: rw.quantity == 1,
            version: 1,
            dynamic_attributes: HashMap::new(),
            source_mutaplasmid_id: None,
            insured_payout_isk: 0.0,
            is_blueprint_copy: rw.blueprint_runs.is_some(),
            blueprint_runs_remaining: rw.blueprint_runs.map(|r| r as i32),
        },
    );
    item_id
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
        // parity: feat:pve.login-campaign, rpc:loginCampaignManager.get_client_campaign_state
        ("loginCampaignManager", "get_client_campaign_state") => {
            Some(handle_login_campaign_get_state(ctx, session))
        }
        // parity: feat:pve.login-campaign, rpc:loginCampaignManager.claim_reward
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
        // parity: rpc:communityTrackMgr.change_qa_mode
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
    let campaigns = get_sde_login_campaigns(&ctx.sde);

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let campaign_id = ext
            .active_login_campaign_id
            .unwrap_or(DEFAULT_LOGIN_CAMPAIGN_ID);
        let campaign = campaigns.get(&campaign_id).ok_or_else(|| {
            err(
                "CampaignNotFound",
                format!("Login campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
            )
        })?;
        let track_length = campaign.track_length();

        let claimed_days = ext
            .login_campaign_claimed_days
            .get(&char_id)
            .cloned()
            .unwrap_or_default();
        let already_claimed_today = claimed_days.contains(&today);
        let num_claimed = claimed_days.len() as i64;
        let next_reward_idx = num_claimed + 1;

        let claimed_reps: Vec<PyRep> = (1..=num_claimed).map(PyRep::Integer).collect();
        let next_reward_timestamp = if already_claimed_today {
            let next_day_unix = (today + 1) * 86_400;
            ((next_day_unix as i64) + 11_644_473_600) * 10_000_000
        } else {
            0
        };

        let mut rewards_by_day = Vec::with_capacity(campaign.alpha_rewards.len());
        for (&day_idx, rw) in &campaign.alpha_rewards {
            let day_1 = i64::from(day_idx) + 1;
            let rw_kv = PyRep::keyval([
                ("typeID", PyRep::Integer(i64::from(rw.type_id))),
                ("quantity", PyRep::Integer(i64::from(rw.quantity))),
                ("tier", PyRep::Integer(i64::from(rw.tier))),
                ("labelMessageID", PyRep::Integer(i64::from(campaign.campaign_name_id))),
            ]);
            rewards_by_day.push((PyRep::Integer(day_1), rw_kv));
        }

        let static_data = PyRep::keyval([
            ("campaign_id", PyRep::Integer(campaign_id)),
            ("titleMessageID", PyRep::Integer(i64::from(campaign.campaign_name_id))),
            ("campaign_duration", PyRep::Integer(track_length)),
            ("is_rookie_campaign", PyRep::Bool(false)),
            ("item_rewards_by_day", PyRep::Dict(rewards_by_day)),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(next_reward_idx)),
            ("num_claimed_rewards", PyRep::Integer(num_claimed)),
            ("reward_count", PyRep::Integer(track_length)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
            ("next_reward_timestamp", PyRep::Integer(next_reward_timestamp)),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(num_claimed)),
            ("total", PyRep::Integer(track_length)),
        ]);

        Ok(PyRep::keyval([
            ("campaign_id", PyRep::Integer(campaign_id)),
            ("campaignID", PyRep::Integer(campaign_id)),
            ("static_data", static_data),
            ("item_progress", item_progress),
            ("bucket_progress", bucket_progress),
            ("reward_event", PyRep::None),
            ("can_claim_today", PyRep::Bool(!already_claimed_today && num_claimed < track_length)),
            ("canClaimToday", PyRep::Bool(!already_claimed_today && num_claimed < track_length)),
            ("current_day", PyRep::Integer(today as i64)),
            ("currentDay", PyRep::Integer(today as i64)),
            ("total_days_claimed", PyRep::Integer(num_claimed)),
            ("totalDaysClaimed", PyRep::Integer(num_claimed)),
        ]))
    })
}

fn handle_login_campaign_claim_reward(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let today = ctx.now_unix() / 86_400;
    let campaigns = get_sde_login_campaigns(&ctx.sde);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let campaign_id = ext
            .active_login_campaign_id
            .unwrap_or(DEFAULT_LOGIN_CAMPAIGN_ID);
        let campaign = campaigns.get(&campaign_id).ok_or_else(|| {
            err(
                "CampaignNotFound",
                format!("Login campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
            )
        })?;
        let track_length = campaign.track_length();

        let (home_station_id, clone_grade) = snap
            .characters
            .get(&char_id)
            .map(|c| {
                let stn = if c.home_station_id > 0 {
                    c.home_station_id
                } else {
                    c.station_id.unwrap_or(60_003_760)
                };
                (stn, c.clone_grade)
            })
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;

        let claimed_days = ext.login_campaign_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Ok(PyRep::None);
        }
        let day_idx = claimed_days.len() as u32;
        if i64::from(day_idx) >= track_length {
            return Ok(PyRep::None);
        }

        let alpha_rw = campaign.alpha_rewards.get(&day_idx);
        let omega_rw = if clone_grade >= 2 {
            campaign.omega_rewards.get(&day_idx)
        } else {
            None
        };
        let primary_rw = alpha_rw.or(omega_rw).ok_or_else(|| {
            err(
                "RewardDefinitionMissing",
                format!("No SDE reward entry for campaign {campaign_id} day index {day_idx}"),
            )
        })?;

        let mut delivered_item_ids = Vec::new();
        if let Some(rw) = alpha_rw {
            delivered_item_ids.push(deliver_reward_entry_to_hangar(snap, char_id, home_station_id, rw));
        }
        if let Some(rw) = omega_rw {
            delivered_item_ids.push(deliver_reward_entry_to_hangar(snap, char_id, home_station_id, rw));
        }

        claimed_days.push(today);
        let reward_index = claimed_days.len() as i64;
        let claimed_reps: Vec<PyRep> = (1..=reward_index).map(PyRep::Integer).collect();
        save_cosmetics_ui_state(snap, &ext);

        let next_day_unix = (today + 1) * 86_400;
        let next_reward_timestamp = ((next_day_unix as i64) + 11_644_473_600) * 10_000_000;

        let reward_item = PyRep::keyval([
            ("campaign_id", PyRep::Integer(campaign_id)),
            ("campaignID", PyRep::Integer(campaign_id)),
            ("reward_index", PyRep::Integer(reward_index)),
            ("typeID", PyRep::Integer(i64::from(primary_rw.type_id))),
            ("type_id", PyRep::Integer(i64::from(primary_rw.type_id))),
            ("quantity", PyRep::Integer(i64::from(primary_rw.quantity))),
            ("tier", PyRep::Integer(i64::from(primary_rw.tier))),
            ("itemID", PyRep::Integer(delivered_item_ids[0])),
            (
                "delivered_item_ids",
                PyRep::List(delivered_item_ids.iter().copied().map(PyRep::Integer).collect()),
            ),
        ]);

        let item_progress = PyRep::keyval([
            ("next_reward_index", PyRep::Integer(reward_index + 1)),
            ("num_claimed_rewards", PyRep::Integer(reward_index)),
            ("reward_count", PyRep::Integer(track_length)),
            ("claimed_rewards", PyRep::List(claimed_reps)),
            ("next_reward_timestamp", PyRep::Integer(next_reward_timestamp)),
        ]);

        let reward_event = PyRep::keyval([
            ("event_id", PyRep::Integer(campaign_id)),
            ("reward_index", PyRep::Integer(reward_index)),
            ("typeID", PyRep::Integer(i64::from(primary_rw.type_id))),
            ("quantity", PyRep::Integer(i64::from(primary_rw.quantity))),
        ]);

        let bucket_progress = PyRep::keyval([
            ("progress", PyRep::Integer(reward_index)),
            ("total", PyRep::Integer(track_length)),
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
    let s = match get_active_seasonal_campaign(now) {
        Some(s) => s,
        None => {
            return Ok(PyRep::Tuple(vec![
                PyRep::None,
                PyRep::None,
                PyRep::None,
                PyRep::None,
            ]));
        }
    };
    let campaigns = get_sde_login_campaigns(&ctx.sde);
    let campaign_id = ctx.db.with_snapshot(|snap| {
        load_cosmetics_ui_state(snap)
            .active_seasonal_campaign_id
            .unwrap_or(s.campaign_id)
    });
    if !campaigns.contains_key(&campaign_id) {
        return Err(err(
            "CampaignNotFound",
            format!("Seasonal campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
        ));
    }

    let start_date_ordinal = GREGORIAN_EPOCH_ORDINAL + (s.start_unix / 86_400) as i64;
    let days_remaining = (s.end_unix.saturating_sub(now) + 86_399) / 86_400;

    Ok(PyRep::Tuple(vec![
        PyRep::Integer(campaign_id),
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
    let campaigns = get_sde_login_campaigns(&ctx.sde);
    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let clone_grade = snap
            .characters
            .get(&char_id)
            .map(|c| c.clone_grade)
            .unwrap_or(1);
        let claimed_days = ext
            .seasonal_claimed_days
            .get(&char_id)
            .cloned()
            .unwrap_or_default();

        let mut entries = Vec::with_capacity(claimed_days.len());
        for (claim_idx, &day_unix) in claimed_days.iter().enumerate() {
            let Some(sched) = SEASONAL_LOGIN_SCHEDULES.iter().find(|s| s.contains(day_unix * 86_400))
            else {
                continue;
            };
            let campaign_id = ext
                .active_seasonal_campaign_id
                .unwrap_or(sched.campaign_id);
            let day_u32 = claim_idx as u32;
            let (alpha_tid, omega_tid) = campaigns
                .get(&campaign_id)
                .map(|c| {
                    let a = c.alpha_rewards.get(&day_u32).map(|r| i64::from(r.type_id));
                    let o = if clone_grade >= 2 {
                        c.omega_rewards.get(&day_u32).map(|r| i64::from(r.type_id))
                    } else {
                        None
                    };
                    (a, o)
                })
                .unwrap_or((None, None));
            let date_ordinal = GREGORIAN_EPOCH_ORDINAL + (day_unix as i64);
            let claim_data = PyRep::keyval([
                ("date_ordinal", PyRep::Integer(date_ordinal)),
                ("campaign_id", PyRep::Integer(campaign_id)),
                ("claimed_timestamp", PyRep::Integer((day_unix * 86_400) as i64)),
                ("clone_grade", PyRep::Integer(i64::from(clone_grade))),
                (
                    "alpha_type_id",
                    alpha_tid.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "omega_type_id",
                    omega_tid.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
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
    let sched = get_active_seasonal_campaign(now).ok_or_else(|| {
        err("NoActiveCampaign", "No seasonal campaign is currently active")
    })?;
    let campaigns = get_sde_login_campaigns(&ctx.sde);

    ctx.db.try_with_mut_snapshot(|snap| {
        let mut ext = load_cosmetics_ui_state(snap);
        let campaign_id = ext
            .active_seasonal_campaign_id
            .unwrap_or(sched.campaign_id);
        let campaign = campaigns.get(&campaign_id).ok_or_else(|| {
            err(
                "CampaignNotFound",
                format!("Seasonal campaign {campaign_id} not found in SDE seasonalcampaignsloginrewards"),
            )
        })?;
        let track_length = campaign.track_length();

        let (home_station_id, clone_grade) = snap
            .characters
            .get(&char_id)
            .map(|c| {
                let stn = if c.home_station_id > 0 {
                    c.home_station_id
                } else {
                    c.station_id.unwrap_or(60_003_760)
                };
                (stn, c.clone_grade)
            })
            .ok_or_else(|| err("CharacterNotFound", format!("Character {char_id} not found")))?;

        let claimed_days = ext.seasonal_claimed_days.entry(char_id).or_default();
        if claimed_days.contains(&today) {
            return Err(err(
                "SeasonalRewardAlreadyClaimedToday",
                "Seasonal reward already claimed for today",
            ));
        }
        let day_idx = claimed_days.len() as u32;
        if i64::from(day_idx) >= track_length {
            return Err(err(
                "CampaignCompleted",
                format!("All {track_length} seasonal rewards in campaign {campaign_id} have been claimed"),
            ));
        }

        let alpha_rw = campaign.alpha_rewards.get(&day_idx);
        let omega_rw = if clone_grade >= 2 {
            campaign.omega_rewards.get(&day_idx)
        } else {
            None
        };
        if alpha_rw.is_none() && omega_rw.is_none() {
            return Err(err(
                "RewardDefinitionMissing",
                format!("No SDE reward entry for seasonal campaign {campaign_id} day index {day_idx}"),
            ));
        }

        if let Some(rw) = alpha_rw {
            deliver_reward_entry_to_hangar(snap, char_id, home_station_id, rw);
        }
        if let Some(rw) = omega_rw {
            deliver_reward_entry_to_hangar(snap, char_id, home_station_id, rw);
        }

        claimed_days.push(today);
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
    if milestone_id <= 0 {
        return Err(err("InvalidMilestoneID", "milestone_id must be positive"));
    }
    let has_logon = ctx.db.with_snapshot(|snap| {
        load_cosmetics_ui_state(snap)
            .milestone_logons
            .contains_key(&char_id)
    });
    if !has_logon {
        return Err(err(
            "MilestonePrerequisiteNotMet",
            "ProcessCharacterLogon must be called before claiming milestone rewards",
        ));
    }

    // DATA GAP: V24 `milestones/client/milestoneSvc.pyj` delegates `ClaimRewards(milestoneID)` to
    // `milestoneMgr`, but milestone timer durations and reward definitions are server-authored and
    // not shipped in client `.fsdbinary` data. Fail closed rather than fake-succeeding without
    // delivering rewards (`R136-P1-NONDELIVERING-CLAIM-FAKE-SUCCESS`).
    Err(err(
        "UnimplementedRPC",
        "milestoneMgr.ClaimRewards: milestone reward definitions are not shipped in V24 client FSD data",
    ))
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

    let templates = get_sde_community_tracks(&ctx.sde);

    ctx.db.with_snapshot(|snap| {
        let ext = load_cosmetics_ui_state(snap);
        let template = templates
            .get(&instance_id)
            .or_else(|| {
                if ext.community_track_points.contains_key(&instance_id) {
                    templates.values().next()
                } else {
                    None
                }
            })
            .ok_or_else(|| {
                err(
                    "CommunityTrackInstanceNotFound",
                    format!(
                        "Community track instance {instance_id} not found in SDE community_track_templates"
                    ),
                )
            })?;

        let raw_progress = ext
            .community_track_points
            .get(&instance_id)
            .copied()
            .unwrap_or(0);
        let progress = raw_progress.clamp(0, i64::from(template.goal));

        // 2-tuple (fsd_identifier, progress) per V24 communitytracks/client/repository.pyj:114.
        Ok(PyRep::Tuple(vec![
            PyRep::Integer(template.template_id),
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
        snap.service_state_blobs
            .insert("community_track_qa_mode".to_string(), qa_mode.to_string());
    });

    Ok(PyRep::Bool(true))
}
